use crate::{
    Commander,
    common::*,
    config::Config,
    crypto::random,
    oauth::{Grant, REFRESH_TTL},
    storage::Expiring,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::Response;

// Stable ChatGPT callback requires RFC 9207 issuer identification; oauth.rs
// advertises it and oauth_tokens.rs sends iss on both success and denial.
pub const CHATGPT_REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const CONNECTION: &str = "p:chatgpt_connection";
const WINDOW: &str = "e:chatgpt_link_window";

#[derive(Clone, Serialize, Deserialize)]
pub struct ConnectionMarker {
    pub client_id: String,
    pub family: String,
}

#[derive(Serialize, Deserialize)]
pub struct Connection {
    client_id: String,
    family: String,
    scope: String,
    linked_at: u64,
}

impl Commander {
    // Initial ChatGPT linking is owner-window gated. Once one connection is
    // pinned, a fresh authorization may reach the administrator-key consent
    // page so ChatGPT's reconnect/refresh flow can replace it without a
    // destructive dashboard reset. The returned marker is persisted with the
    // authorization and compared again at approval and token exchange.
    pub async fn connection_authorization_context(
        &self,
        config: &Config,
    ) -> ApiResult<Option<ConnectionMarker>> {
        if !config.chatgpt_only {
            return Ok(None);
        }
        if let Some(connection) = self.state.storage().get::<Connection>(CONNECTION).await? {
            return Ok(Some(ConnectionMarker {
                client_id: connection.client_id,
                family: connection.family,
            }));
        }
        if !self.get::<bool>(WINDOW).await?.unwrap_or(false) {
            return Err(ApiError::new(
                403,
                "linking_closed",
                "Open a ten-minute ChatGPT linking window in the owner dashboard first.",
            ));
        }
        Ok(None)
    }

    pub async fn connection_authorization_matches(
        &self,
        expected: Option<&ConnectionMarker>,
        config: &Config,
    ) -> ApiResult<bool> {
        if !config.chatgpt_only {
            return Ok(true);
        }
        let current = self.state.storage().get::<Connection>(CONNECTION).await?;
        match (expected, current) {
            (Some(expected), Some(current)) => {
                Ok(expected.client_id == current.client_id && expected.family == current.family)
            }
            (None, None) => Ok(self.get::<bool>(WINDOW).await?.unwrap_or(false)),
            _ => Ok(false),
        }
    }

    pub async fn pin_connection(
        &self,
        grant: &Grant,
        expected: Option<&ConnectionMarker>,
        config: &Config,
    ) -> ApiResult<()> {
        if !config.chatgpt_only {
            return Ok(());
        }
        if !self
            .connection_authorization_matches(expected, config)
            .await?
        {
            return Err(ApiError::new(
                400,
                "invalid_grant",
                "The ChatGPT connection changed while authorization was pending",
            ));
        }
        if let Some(previous) = expected {
            // Invalidate the old family before publishing the replacement pin.
            // The mutation gate serializes token exchange, so two replacement
            // codes created from the same old connection cannot both succeed.
            self.put(&format!("e:family:{}", previous.family), true, REFRESH_TTL)
                .await?;
        }
        self.state
            .storage()
            .put(
                CONNECTION,
                Connection {
                    client_id: grant.client_id.clone(),
                    family: grant.family.clone(),
                    scope: grant.scope.clone(),
                    linked_at: now(),
                },
            )
            .await?;
        self.state.storage().delete(WINDOW).await?;
        self.audit(
            if expected.is_some() {
                "chatgpt_reconnected"
            } else {
                "chatgpt_connected"
            },
            None,
            "success",
        )
        .await?;
        Ok(())
    }

    pub async fn connection_matches(&self, grant: &Grant, config: &Config) -> ApiResult<bool> {
        if !config.chatgpt_only {
            return Ok(true);
        }
        Ok(self
            .state
            .storage()
            .get::<Connection>(CONNECTION)
            .await?
            .is_some_and(|c| c.client_id == grant.client_id && c.family == grant.family))
    }

    pub async fn connection_status(&self, config: &Config) -> ApiResult<Response> {
        let connection = self.state.storage().get::<Connection>(CONNECTION).await?;
        let window = self
            .state
            .storage()
            .get::<Expiring<bool>>(WINDOW)
            .await?
            .filter(|w| w.expires > now());
        let status = if connection.is_some() {
            "linked"
        } else if window.is_some() {
            "awaiting_chatgpt"
        } else {
            "locked"
        };
        json(&json!({"chatgpt_only":config.chatgpt_only,"status":status,
            "linking_expires_at":window.map(|w| w.expires),
            "linked_at":connection.as_ref().map(|c| c.linked_at),
            "scope":connection.as_ref().map(|c| &c.scope)}))
    }

    pub async fn reset_connection(&self, open: bool) -> ApiResult<Response> {
        // Epoch invalidation precedes clearing the pin. Every outstanding code,
        // consent ticket, access token and refresh token becomes unusable.
        self.state.storage().put("p:auth_epoch", random()?).await?;
        self.state.storage().delete(CONNECTION).await?;
        self.state.storage().delete(WINDOW).await?;
        if open {
            self.put(WINDOW, true, 600).await?;
        }
        self.audit(
            if open {
                "chatgpt_linking_opened"
            } else {
                "clients_revoked"
            },
            None,
            "success",
        )
        .await?;
        json(&json!({"revoked":true,"linking_open":open}))
    }
}

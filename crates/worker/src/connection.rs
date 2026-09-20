use crate::{
    Commander, common::*, config::Config, crypto::random, oauth::Grant, storage::Expiring,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::Response;

// Stable ChatGPT callback requires RFC 9207 issuer identification; oauth.rs
// advertises it and oauth_tokens.rs sends iss on both success and denial.
pub const CHATGPT_REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const CONNECTION: &str = "p:chatgpt_connection";
const WINDOW: &str = "e:chatgpt_link_window";

#[derive(Serialize, Deserialize)]
pub struct Connection {
    client_id: String,
    family: String,
    scope: String,
    linked_at: u64,
}

impl Commander {
    pub async fn connection_open(&self, config: &Config) -> ApiResult<()> {
        if !config.chatgpt_only {
            return Ok(());
        }
        if self
            .state
            .storage()
            .get::<Connection>(CONNECTION)
            .await?
            .is_some()
        {
            return Err(ApiError::new(
                403,
                "connection_locked",
                "A ChatGPT connection is already approved. Reset access in the owner dashboard to link again.",
            ));
        }
        if !self.get::<bool>(WINDOW).await?.unwrap_or(false) {
            return Err(ApiError::new(
                403,
                "linking_closed",
                "Open a ten-minute ChatGPT linking window in the owner dashboard first.",
            ));
        }
        Ok(())
    }

    pub async fn pin_connection(&self, grant: &Grant, config: &Config) -> ApiResult<()> {
        if !config.chatgpt_only {
            return Ok(());
        }
        self.connection_open(config).await?;
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
        self.audit("chatgpt_connected", None, "success").await?;
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

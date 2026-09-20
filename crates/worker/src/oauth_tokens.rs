use crate::{
    Commander,
    common::*,
    config::Config,
    connection::CHATGPT_REDIRECT,
    crypto::*,
    oauth::{Authorization, Client, Grant, REFRESH_TTL, Refresh},
};
use serde_json::json;
use worker::{Request, Response, Url};

fn invalid_grant() -> ApiError {
    ApiError::new(
        400,
        "invalid_grant",
        "Grant expired, already used, or does not match this client",
    )
}

impl Commander {
    pub async fn approve_client(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        self.rate("approve", 20).await?;
        require(
            req.headers().get("Origin")?.as_deref() == Some(config.origin.as_str()),
            "Same-origin approval required",
        )?;
        let values = form(req).await?;
        if !secret_equal(param(&values, "admin_token")?, &config.admin) {
            return Err(ApiError::new(
                401,
                "unauthorized",
                "Administrator credential required",
            ));
        }
        let key = format!("e:consent:{}", hash(param(&values, "ticket")?));
        let authorization = self
            .get::<Authorization>(&key)
            .await?
            .ok_or_else(invalid_grant)?;
        let epoch: String = self
            .state
            .storage()
            .get("p:auth_epoch")
            .await?
            .unwrap_or_default();
        if authorization.epoch != epoch {
            return Err(invalid_grant());
        }
        self.connection_open(config).await?;
        require(
            !config.chatgpt_only || authorization.redirect_uri == CHATGPT_REDIRECT,
            "Only the exact ChatGPT OAuth callback is allowed",
        )?;
        let decision = param(&values, "decision")?;
        require(
            decision == "allow" || decision == "deny",
            "Invalid decision",
        )?;
        self.state.storage().delete(&key).await?;
        let mut redirect = Url::parse(&authorization.redirect_uri)?;
        if decision == "allow" {
            let code = random()?;
            self.put(&format!("e:code:{}", hash(&code)), &authorization, 60)
                .await?;
            redirect.query_pairs_mut().append_pair("code", &code);
        } else {
            redirect
                .query_pairs_mut()
                .append_pair("error", "access_denied");
        }
        redirect
            .query_pairs_mut()
            .append_pair("state", &authorization.state)
            .append_pair("iss", &config.origin);
        // Response::redirect creates immutable Fetch headers. Build the redirect
        // explicitly so the shared security-header middleware can decorate it.
        let mut response = empty(303)?;
        response.headers_mut().set("Location", redirect.as_str())?;
        Ok(response)
    }

    async fn issue_tokens(&self, grant: Grant) -> ApiResult<Response> {
        let access_token = random()?;
        let refresh_token = random()?;
        self.put(&format!("e:family:{}", grant.family), false, REFRESH_TTL)
            .await?;
        self.put(&format!("e:access:{}", hash(&access_token)), &grant, 900)
            .await?;
        self.put(
            &format!("e:refresh:{}", hash(&refresh_token)),
            Refresh {
                grant: grant.clone(),
                used: false,
            },
            REFRESH_TTL,
        )
        .await?;
        json(
            &json!({"access_token":access_token,"refresh_token":refresh_token,"token_type":"Bearer","expires_in":900,"scope":grant.scope}),
        )
    }

    pub async fn exchange_token(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        self.rate("token", 120).await?;
        let values = form(req).await?;
        let client_id = param(&values, "client_id")?;
        let client = self
            .get::<Client>(&format!("e:client:{client_id}"))
            .await?
            .ok_or_else(invalid_grant)?;
        require(
            param(&values, "resource")? == config.resource(),
            "Resource does not match this MCP server",
        )?;
        let epoch: String = self
            .state
            .storage()
            .get("p:auth_epoch")
            .await?
            .unwrap_or_default();
        let grant = match param(&values, "grant_type")? {
            "authorization_code" => {
                self.connection_open(config).await?;
                let key = format!("e:code:{}", hash(param(&values, "code")?));
                let authorization = self
                    .get::<Authorization>(&key)
                    .await?
                    .ok_or_else(invalid_grant)?;
                let verifier = param(&values, "code_verifier")?;
                require(
                    (43..=128).contains(&verifier.len())
                        && verifier
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)),
                    "Invalid PKCE verifier",
                )?;
                if authorization.client_id != client_id
                    || authorization.epoch != epoch
                    || authorization.redirect_uri != param(&values, "redirect_uri")?
                    || authorization.resource != config.resource()
                    || (config.chatgpt_only && authorization.redirect_uri != CHATGPT_REDIRECT)
                    || !secret_equal(&authorization.challenge, &hash(verifier))
                {
                    return Err(invalid_grant());
                }
                self.state.storage().delete(&key).await?;
                let grant = Grant {
                    client_id: client_id.into(),
                    scope: authorization.scope,
                    family: random()?,
                    epoch,
                };
                self.pin_connection(&grant, config).await?;
                grant
            }
            "refresh_token" => {
                let key = format!("e:refresh:{}", hash(param(&values, "refresh_token")?));
                let mut refresh = self.get::<Refresh>(&key).await?.ok_or_else(invalid_grant)?;
                if refresh.grant.client_id != client_id {
                    return Err(invalid_grant());
                }
                if refresh.used {
                    self.put(
                        &format!("e:family:{}", refresh.grant.family),
                        true,
                        REFRESH_TTL,
                    )
                    .await?;
                    return Err(invalid_grant());
                }
                let revoked = self
                    .get::<bool>(&format!("e:family:{}", refresh.grant.family))
                    .await?
                    .unwrap_or(true);
                if revoked
                    || refresh.grant.epoch != epoch
                    || !self.connection_matches(&refresh.grant, config).await?
                {
                    return Err(invalid_grant());
                }
                if let Some(scope) = values.get("scope") {
                    let scopes: Vec<_> = scope.split_whitespace().collect();
                    require(
                        !scopes.is_empty()
                            && scopes.iter().all(|s| {
                                refresh.grant.scope.split_whitespace().any(|old| old == *s)
                            }),
                        "Refresh cannot expand scopes",
                    )?;
                    refresh.grant.scope = scopes.join(" ");
                }
                refresh.used = true;
                self.put(&key, &refresh, REFRESH_TTL).await?;
                refresh.grant
            }
            _ => {
                return Err(ApiError::new(
                    400,
                    "unsupported_grant_type",
                    "Unsupported grant_type",
                ));
            }
        };
        self.put(&format!("e:client:{client_id}"), client, REFRESH_TTL)
            .await?;
        self.issue_tokens(grant).await
    }

    pub async fn revoke_token(&self, req: &mut Request) -> ApiResult<Response> {
        self.rate("revoke", 60).await?;
        let values = form(req).await?;
        let token_hash = hash(param(&values, "token")?);
        let grant =
            if let Some(grant) = self.get::<Grant>(&format!("e:access:{token_hash}")).await? {
                Some(grant)
            } else {
                self.get::<Refresh>(&format!("e:refresh:{token_hash}"))
                    .await?
                    .map(|r| r.grant)
            };
        if let Some(grant) = grant {
            self.put(&format!("e:family:{}", grant.family), true, REFRESH_TTL)
                .await?;
        }
        empty(200)
    }
}

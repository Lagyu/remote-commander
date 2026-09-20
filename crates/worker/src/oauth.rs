use crate::{
    Commander,
    common::*,
    config::{Config, bearer},
    connection::CHATGPT_REDIRECT,
    crypto::*,
    storage::Expiring,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use worker::{Request, Response, Url};

pub const REFRESH_TTL: u64 = 30 * 24 * 3600;

#[derive(Clone, Serialize, Deserialize)]
pub struct Client {
    pub name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Authorization {
    pub client_id: String,
    pub redirect_uri: String,
    pub challenge: String,
    pub scope: String,
    pub state: String,
    pub resource: String,
    #[serde(default)]
    pub epoch: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Grant {
    pub client_id: String,
    pub scope: String,
    pub family: String,
    pub epoch: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Refresh {
    pub grant: Grant,
    pub used: bool,
}

pub fn metadata(config: &Config, resource: bool) -> ApiResult<Response> {
    if resource {
        json(
            &json!({"resource":config.resource(),"authorization_servers":[config.origin],"scopes_supported":rdc_protocol::SCOPES,"bearer_methods_supported":["header"]}),
        )
    } else {
        json(&json!({
            "issuer":config.origin,
            "authorization_response_iss_parameter_supported":true,
            "authorization_endpoint":format!("{}/oauth/authorize",config.origin),
            "token_endpoint":format!("{}/oauth/token",config.origin),
            "registration_endpoint":format!("{}/oauth/register",config.origin),
            "revocation_endpoint":format!("{}/oauth/revoke",config.origin),
            "response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],
            "code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"],
            "scopes_supported":rdc_protocol::SCOPES
        }))
    }
}

impl Commander {
    pub async fn register_client(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        self.rate("register", 10).await?;
        self.connection_open(config).await?;
        let value: Value = json_body(req).await?;
        let redirects = value["redirect_uris"]
            .as_array()
            .ok_or_else(|| ApiError::bad("redirect_uris is required"))?;
        require(
            !redirects.is_empty() && redirects.len() <= 8,
            "Register 1–8 redirect URIs",
        )?;
        require(
            value["token_endpoint_auth_method"]
                .as_str()
                .unwrap_or("none")
                == "none",
            "Only public PKCE clients are supported",
        )?;
        let name = value["client_name"].as_str().unwrap_or("MCP client");
        require(
            !name.trim().is_empty() && name.len() <= 80,
            "Invalid client_name",
        )?;
        let mut redirect_uris = Vec::new();
        for redirect in redirects {
            let redirect = redirect
                .as_str()
                .ok_or_else(|| ApiError::bad("Redirect URI must be a string"))?;
            require(redirect.len() <= 2048, "Redirect URI is too long")?;
            require(
                !config.chatgpt_only || redirect == CHATGPT_REDIRECT,
                "Only the exact ChatGPT OAuth callback is allowed",
            )?;
            let uri = Url::parse(redirect)?;
            let local =
                config.local && matches!(uri.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            require(
                (uri.scheme() == "https" || (local && uri.scheme() == "http"))
                    && uri.host_str().is_some()
                    && uri.username().is_empty()
                    && uri.password().is_none()
                    && uri.fragment().is_none(),
                "Redirect URI must be HTTPS without userinfo or fragment",
            )?;
            redirect_uris.push(redirect.to_owned());
        }
        let active = self
            .list::<Expiring<Client>>("e:client:", 256)
            .await?
            .into_iter()
            .filter(|(_, v)| v.expires > now())
            .count();
        require(active < 128, "Client registration capacity reached")?;
        let id = random()?;
        let client = Client {
            name: name.to_owned(),
            redirect_uris,
        };
        self.put(&format!("e:client:{id}"), &client, REFRESH_TTL)
            .await?;
        Ok(json(&json!({"client_id":id,"client_name":client.name,"redirect_uris":client.redirect_uris,"client_id_issued_at":now(),"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}))?.with_status(201))
    }

    pub async fn authorize(&self, req: &Request, config: &Config) -> ApiResult<Response> {
        self.rate("authorize", 20).await?;
        self.connection_open(config).await?;
        let mut query = BTreeMap::new();
        for (key, value) in req.url()?.query_pairs() {
            require(
                query.insert(key.into_owned(), value.into_owned()).is_none(),
                "Duplicate authorization parameter",
            )?;
        }
        let client_id = param(&query, "client_id")?;
        let client = self
            .get::<Client>(&format!("e:client:{client_id}"))
            .await?
            .ok_or_else(|| ApiError::bad("Unknown client"))?;
        let redirect_uri = param(&query, "redirect_uri")?;
        require(
            !config.chatgpt_only || redirect_uri == CHATGPT_REDIRECT,
            "Only the exact ChatGPT OAuth callback is allowed",
        )?;
        require(
            client.redirect_uris.iter().any(|uri| uri == redirect_uri),
            "Unregistered redirect_uri",
        )?;
        require(
            param(&query, "response_type")? == "code",
            "Only authorization code is supported",
        )?;
        require(
            param(&query, "code_challenge_method")? == "S256",
            "PKCE S256 is required",
        )?;
        let challenge = param(&query, "code_challenge")?;
        require(
            challenge.len() == 43
                && challenge
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid PKCE challenge",
        )?;
        let resource = param(&query, "resource")?;
        require(
            resource == config.resource(),
            "Resource does not match this MCP server",
        )?;
        let scope = query
            .get("scope")
            .map(String::as_str)
            .unwrap_or("commander:read");
        // ChatGPT concatenates base and action/default scopes. Repeated scope
        // names grant no additional authority and must be treated as a set.
        let mut seen = BTreeSet::new();
        let scopes: Vec<_> = scope
            .split_whitespace()
            .filter(|s| seen.insert(*s))
            .collect();
        require(
            !scopes.is_empty()
                && scopes.len() <= 3
                && scopes.iter().all(|s| rdc_protocol::SCOPES.contains(s)),
            "Unknown scope",
        )?;
        let state = query.get("state").cloned().unwrap_or_default();
        require(state.len() <= 512, "State is too long")?;
        let authorization = Authorization {
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            challenge: challenge.into(),
            scope: scopes.join(" "),
            state,
            resource: resource.into(),
            epoch: self
                .state
                .storage()
                .get("p:auth_epoch")
                .await?
                .unwrap_or_default(),
        };
        let ticket = random()?;
        self.put(&format!("e:consent:{}", hash(&ticket)), &authorization, 300)
            .await?;
        html(format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Authorize access · Remote Commander</title><link rel="stylesheet" href="/app.css"></head><body><main class="consent"><a class="brand" href="/">Remote Commander</a><p class="eyebrow">CLIENT AUTHORIZATION</p><h1>Connect {}</h1><p>This client is requesting access to your paired computers.</p><dl><dt>Requested permissions</dt><dd>{}</dd><dt>Return address</dt><dd class="mono">{}</dd></dl><p>Read permits browsing files. Write permits file changes. Execute permits shell sessions with your OS user's authority when locally enabled.</p><form method="post" action="/oauth/approve"><input type="hidden" name="ticket" value="{}"><label for="secret">Administrator key</label><input id="secret" name="admin_token" type="password" autocomplete="current-password" required minlength="32"><div class="actions"><button type="submit" name="decision" value="allow">Authorize client</button><button class="secondary" type="submit" name="decision" value="deny">Deny</button></div></form><p class="muted">This approval expires in five minutes. Only authorize a client you intended to connect.</p></main></body></html>"#,
            escape(&client.name),
            escape(&authorization.scope),
            escape(&authorization.redirect_uri),
            escape(&ticket)
        ))
    }

    pub async fn authenticate(&self, req: &Request, config: &Config) -> ApiResult<Grant> {
        let token = bearer(req)?;
        let grant = self
            .get::<Grant>(&format!("e:access:{}", hash(&token)))
            .await?
            .ok_or_else(|| {
                ApiError::new(401, "invalid_token", "Access token expired or invalid")
            })?;
        let epoch: String = self
            .state
            .storage()
            .get("p:auth_epoch")
            .await?
            .unwrap_or_default();
        let revoked = self
            .get::<bool>(&format!("e:family:{}", grant.family))
            .await?
            .unwrap_or(true);
        if revoked || grant.epoch != epoch || !self.connection_matches(&grant, config).await? {
            return Err(ApiError::new(
                401,
                "invalid_token",
                "Client authorization was revoked",
            ));
        }
        Ok(grant)
    }
}

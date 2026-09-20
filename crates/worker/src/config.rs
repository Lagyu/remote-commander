use crate::{common::*, crypto::secret_equal};
use worker::{Env, Request, Url};

pub struct Config {
    pub origin: String,
    pub admin: String,
    pub local: bool,
    pub chatgpt_only: bool,
}

impl Config {
    pub fn read(env: &Env) -> ApiResult<Self> {
        let origin = env
            .var("PUBLIC_URL")
            .map_err(|_| ApiError::new(503, "not_configured", "PUBLIC_URL is required"))?
            .to_string();
        let parsed = Url::parse(&origin)?;
        let local = env
            .var("ALLOW_LOCALHOST")
            .map(|v| v.to_string() == "true")
            .unwrap_or(false);
        let is_loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        require(
            parsed.scheme() == "https" || (local && is_loopback && parsed.scheme() == "http"),
            "PUBLIC_URL must use HTTPS",
        )?;
        require(
            parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.path() == "/"
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "PUBLIC_URL must be an origin",
        )?;
        let admin = env
            .secret("ADMIN_TOKEN")
            .map_err(|_| {
                ApiError::new(
                    503,
                    "not_configured",
                    "Administrator credential is not configured",
                )
            })?
            .to_string();
        if admin.len() < 32 {
            return Err(ApiError::new(
                503,
                "not_configured",
                "Administrator credential must have at least 32 characters",
            ));
        }
        Ok(Self {
            origin: parsed.origin().ascii_serialization(),
            admin,
            local,
            // Public deployments always require the owner-pinned ChatGPT flow.
            // Generic OAuth clients are available only for explicit local tests.
            chatgpt_only: !(local && is_loopback)
                || env
                    .var("CHATGPT_ONLY")
                    .map(|v| v.to_string() != "false")
                    .unwrap_or(true),
        })
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    pub fn challenge(&self, error: &str) -> String {
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", error=\"{}\"",
            self.origin, error
        )
    }

    pub fn check_origin(&self, req: &Request) -> ApiResult<()> {
        require(
            req.url()?.origin().ascii_serialization() == self.origin,
            "Request host does not match PUBLIC_URL",
        )?;
        if let Some(origin) = req.headers().get("Origin")?
            && origin != self.origin
        {
            return Err(ApiError::new(
                403,
                "invalid_origin",
                "Origin is not allowed",
            ));
        }
        Ok(())
    }

    pub fn check_admin(&self, req: &Request) -> ApiResult<()> {
        if secret_equal(&bearer(req)?, &self.admin) {
            Ok(())
        } else {
            Err(ApiError::new(
                401,
                "unauthorized",
                "Administrator credential required",
            ))
        }
    }
}

pub fn bearer(req: &Request) -> ApiResult<String> {
    let value = req.headers().get("Authorization")?.unwrap_or_default();
    let token = value
        .strip_prefix("Bearer ")
        .filter(|v| v.len() >= 32 && v.len() <= 256 && !v.contains(char::is_whitespace));
    token
        .map(str::to_owned)
        .ok_or_else(|| ApiError::new(401, "invalid_token", "Bearer token required"))
}

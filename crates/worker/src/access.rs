//! Cloudflare authenticates the owner at the edge. Trust only the runtime's
//! Access context, never user-supplied identity/JWT headers or cookies.
use crate::common::{ApiError, ApiResult};
use js_sys::{Function, Promise, Reflect};
use worker::{Context, Env, Method, Request, Url, wasm_bindgen::JsCast};

fn protocol_request(req: &Request) -> bool {
    let path = req.path();
    match req.method() {
        Method::Get => {
            matches!(
                path.as_str(),
                "/health"
                    | "/mcp"
                    | "/.well-known/oauth-protected-resource"
                    | "/.well-known/oauth-protected-resource/mcp"
                    | "/.well-known/oauth-authorization-server"
            ) || path.strip_prefix("/agent/").is_some_and(|id| {
                id.len() == 43
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
        }
        Method::Post => matches!(
            path.as_str(),
            "/mcp"
                | "/oauth/register"
                | "/oauth/token"
                | "/oauth/revoke"
                | "/pair/start"
                | "/pair/token"
        ),
        Method::Options => path == "/mcp",
        _ => false,
    }
}

pub async fn enforce(req: &Request, env: &Env, ctx: &Context) -> ApiResult<()> {
    if protocol_request(req) {
        return Ok(());
    }
    let value = |name| env.var(name).map(|v| v.to_string()).unwrap_or_default();
    let local = value("ALLOW_LOCALHOST") == "true"
        && Url::parse(&value("PUBLIC_URL")).is_ok_and(|url| {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                && req
                    .url()
                    .is_ok_and(|request| request.origin() == url.origin())
        });
    if local && value("ACCESS_REQUIRED") != "true" {
        return Ok(());
    }
    let audience = value("ACCESS_AUD");
    let owner = value("ACCESS_OWNER_EMAIL");
    if audience.len() != 64
        || !audience.bytes().all(|b| b.is_ascii_hexdigit())
        || !owner.contains('@')
        || owner.len() > 254
    {
        return Err(ApiError::new(
            503,
            "access_not_configured",
            "Administration is unavailable until Cloudflare Access is configured",
        ));
    }
    let denied = || {
        ApiError::new(
            403,
            "access_required",
            "The owner's Cloudflare Access session is required",
        )
    };
    // ctx.access is populated only by Cloudflare, before the Worker executes.
    // Validate here: Access context is not propagated to Durable Objects.
    let access = Reflect::get(ctx.as_ref(), &"access".into()).map_err(|_| denied())?;
    if access.is_null() || access.is_undefined() {
        return Err(denied());
    }
    let actual_audience = Reflect::get(&access, &"aud".into()).map_err(|_| denied())?;
    if actual_audience.as_string().as_deref() != Some(audience.as_str()) {
        return Err(denied());
    }
    let get_identity = Reflect::get(&access, &"getIdentity".into())
        .map_err(|_| denied())?
        .dyn_into::<Function>()
        .map_err(|_| denied())?;
    let promise = Promise::resolve(&get_identity.call0(&access).map_err(|_| denied())?);
    let identity = worker::wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| denied())?;
    let email = Reflect::get(&identity, &"email".into()).map_err(|_| denied())?;
    if !email
        .as_string()
        .is_some_and(|email| email.eq_ignore_ascii_case(&owner))
    {
        return Err(denied());
    }
    Ok(())
}

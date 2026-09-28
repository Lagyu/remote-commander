mod access;
mod common;
mod config;
mod connection;
mod crypto;
mod downloads;
mod mcp;
mod oauth;
mod oauth_tokens;
mod pairing;
mod relay;
mod storage;

use common::*;
use config::Config;
use futures::lock::Mutex;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use worker::*;

#[event(fetch)]
async fn main(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Reject private requests before buffering their body or waking the DO.
    if let Err(error) = access::enforce(&req, &env, &ctx).await {
        let mut response = error.response()?;
        let headers = response.headers_mut();
        headers.set("Cache-Control", "no-store")?;
        headers.set("X-Content-Type-Options", "nosniff")?;
        headers.set(
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'",
        )?;
        headers.set("Referrer-Policy", "same-origin")?;
        if req.url()?.scheme() == "https" {
            headers.set("Strict-Transport-Security", "max-age=31536000")?;
        }
        return Ok(response);
    }
    // Buffer bounded uploads before crossing the DO boundary. Forwarding a live
    // stream permits an early DO rejection to outlive the source Fetch event.
    // Receiving here also keeps slow uploads outside the authorization lock.
    if req.inner().body().is_some() {
        let limit = if downloads::is_upload_chunk(&req) {
            rdc_protocol::TRANSFER_CHUNK_BYTES
        } else {
            rdc_protocol::MAX_REQUEST_BYTES
        };
        let bytes = match bounded_bytes_limit(&mut req, limit).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let mut response = error.response()?;
                response.headers_mut().set("Cache-Control", "no-store")?;
                response
                    .headers_mut()
                    .set("X-Content-Type-Options", "nosniff")?;
                return Ok(response);
            }
        };
        let init = RequestInit {
            method: req.method(),
            headers: req.headers().clone(),
            body: Some(js_sys::Uint8Array::from(bytes.as_slice()).into()),
            redirect: RequestRedirect::Manual,
            ..RequestInit::default()
        };
        req = Request::new_with_init(req.url()?.as_str(), &init)?;
    }
    env.durable_object("COMMANDER")?
        .id_from_name("single-owner-v1")?
        .get_stub()?
        .fetch_with_request(req)
        .await
}

#[durable_object]
pub struct Commander {
    state: State,
    env: Env,
    gate: Mutex<()>,
    pending: Rc<RefCell<HashMap<String, relay::Pending>>>,
}

impl Commander {
    async fn route(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        config.check_origin(req)?;
        require(req.url()?.as_str().len() <= 8192, "Request URL is too long")?;
        let path = req.path();
        let method = req.method();
        if method == Method::Options {
            let mut response = empty(204)?;
            response
                .headers_mut()
                .set("Access-Control-Allow-Origin", &config.origin)?;
            response
                .headers_mut()
                .set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")?;
            response.headers_mut().set(
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type, MCP-Protocol-Version",
            )?;
            response.headers_mut().set("Vary", "Origin")?;
            return Ok(response);
        }
        if path == "/mcp" {
            return self.mcp(req, config).await;
        }
        if method == Method::Get {
            match path.as_str() {
                "/" => return html(include_str!("../../../web/index.html")),
                "/app.js" => {
                    let mut response = Response::ok(include_str!("../../../web/app.js"))?;
                    response
                        .headers_mut()
                        .set("Content-Type", "text/javascript; charset=utf-8")?;
                    return Ok(response);
                }
                "/app.css" => {
                    let mut response = Response::ok(include_str!("../../../web/app.css"))?;
                    response
                        .headers_mut()
                        .set("Content-Type", "text/css; charset=utf-8")?;
                    return Ok(response);
                }
                "/health" => {
                    return json(
                        &serde_json::json!({"ok":true,"service":"remote-commander","version":env!("CARGO_PKG_VERSION"),"file_transfers":{"max_bytes":rdc_protocol::MAX_TRANSFER_BYTES,"chunk_bytes":rdc_protocol::TRANSFER_CHUNK_BYTES,"expires_in":rdc_protocol::TRANSFER_TTL_SECONDS}}),
                    );
                }
                "/.well-known/oauth-protected-resource"
                | "/.well-known/oauth-protected-resource/mcp" => {
                    return oauth::metadata(config, true);
                }
                "/.well-known/oauth-authorization-server" => return oauth::metadata(config, false),
                _ => {}
            }
        }
        if downloads::is_transfer_request(req) {
            return self.transfer_route(req).await;
        }
        let _guard = self.gate.lock().await;
        if path.starts_with("/api/") {
            return self.admin_route(req, config).await;
        }
        if method == Method::Get && path.starts_with("/agent/") {
            return self.connect_agent(req).await;
        }
        match (method, path.as_str()) {
            (Method::Post, "/pair/start") => self.start_pairing(req, config).await,
            (Method::Post, "/pair/token") => self.redeem_pairing(req).await,
            (Method::Post, "/oauth/register") => self.register_client(req, config).await,
            (Method::Get, "/oauth/authorize") => self.authorize(req, config).await,
            (Method::Post, "/oauth/approve") => self.approve_client(req, config).await,
            (Method::Post, "/oauth/token") => self.exchange_token(req, config).await,
            (Method::Post, "/oauth/revoke") => self.revoke_token(req).await,
            _ => Err(ApiError::new(404, "not_found", "Endpoint not found")),
        }
    }
}

impl DurableObject for Commander {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            gate: Mutex::new(()),
            pending: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let config = Config::read(&self.env);
        let mut response = match config.as_ref() {
            Ok(config) => match self.route(&mut req, config).await {
                Ok(response) => response,
                Err(error) => {
                    let mut response = error.response()?;
                    if error.status == 401 && req.path() == "/mcp" {
                        response
                            .headers_mut()
                            .set("WWW-Authenticate", &config.challenge("invalid_token"))?;
                    }
                    if error.status == 429 {
                        response.headers_mut().set("Retry-After", "60")?;
                    }
                    response
                }
            },
            Err(error) => error.response()?,
        };
        if response.status_code() != 101 {
            let headers = response.headers_mut();
            headers.set("Cache-Control", "no-store")?;
            headers.set("X-Content-Type-Options", "nosniff")?;
            // no-referrer makes Chromium send Origin: null for HTML form POSTs.
            // Keep same-origin CSRF validation without leaking cross-site URLs.
            headers.set("Referrer-Policy", "same-origin")?;
            // Chromium checks form-action again on the OAuth 303 destination.
            // Allow the exact ChatGPT callback as well as same-origin forms.
            headers.set("Content-Security-Policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self' https://chatgpt.com/connector_platform_oauth_redirect; frame-ancestors 'none'; base-uri 'none'")?;
            headers.set(
                "Permissions-Policy",
                "camera=(), microphone=(), geolocation=()",
            )?;
            if config
                .as_ref()
                .is_ok_and(|c| c.origin.starts_with("https:"))
            {
                headers.set("Strict-Transport-Security", "max-age=31536000")?;
            }
        }
        Ok(response)
    }

    async fn websocket_message(
        &self,
        socket: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        self.receive_agent(socket, message)
    }

    async fn websocket_close(
        &self,
        socket: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        self.end_socket(&socket, "device_disconnected");
        Ok(())
    }

    async fn websocket_error(&self, socket: WebSocket, _error: Error) -> Result<()> {
        self.end_socket(&socket, "connection_error");
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        let _guard = self.gate.lock().await;
        match self.cleanup().await {
            Ok(()) => Response::empty(),
            Err(error) => error.response(),
        }
    }
}

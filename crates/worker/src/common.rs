use futures::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use worker::{Request, Response};

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message)
    }
    pub fn internal() -> Self {
        Self::new(500, "internal_error", "The request could not be completed")
    }
    pub fn response(&self) -> worker::Result<Response> {
        Ok(
            Response::from_json(&json!({"error":self.code,"error_description":self.message}))?
                .with_status(self.status),
        )
    }
}
impl From<worker::Error> for ApiError {
    fn from(_: worker::Error) -> Self {
        Self::internal()
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(_: serde_json::Error) -> Self {
        Self::bad("Malformed JSON or invalid parameter types")
    }
}
impl From<url::ParseError> for ApiError {
    fn from(_: url::ParseError) -> Self {
        Self::bad("Invalid URL")
    }
}

pub fn require(condition: bool, message: &str) -> ApiResult<()> {
    if condition {
        Ok(())
    } else {
        Err(ApiError::bad(message))
    }
}
pub fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}
pub fn json(value: &impl Serialize) -> ApiResult<Response> {
    Ok(Response::from_json(value)?)
}
pub fn empty(status: u16) -> ApiResult<Response> {
    Ok(Response::empty()?.with_status(status))
}
pub fn html(value: impl Into<String>) -> ApiResult<Response> {
    Ok(Response::from_html(value.into())?)
}

pub async fn body(req: &mut Request, content_type: &str) -> ApiResult<Vec<u8>> {
    let actual = req.headers().get("Content-Type")?.unwrap_or_default();
    require(
        actual.split(';').next().unwrap_or_default().trim() == content_type,
        "Unsupported Content-Type",
    )?;
    bounded_bytes(req).await
}

pub async fn bounded_bytes(req: &mut Request) -> ApiResult<Vec<u8>> {
    bounded_bytes_limit(req, rdc_protocol::MAX_REQUEST_BYTES).await
}

pub async fn bounded_bytes_limit(req: &mut Request, limit: usize) -> ApiResult<Vec<u8>> {
    if let Some(length) = req.headers().get("Content-Length")? {
        let length = length
            .parse::<usize>()
            .map_err(|_| ApiError::bad("Invalid Content-Length"))?;
        if length > limit {
            return Err(ApiError::new(
                413,
                "request_too_large",
                format!("Request exceeds {limit} bytes"),
            ));
        }
    }
    if req.inner().body().is_none() {
        return Ok(Vec::new());
    }
    let receive = async {
        let mut bytes = Vec::new();
        let mut stream = req.stream()?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len() + chunk.len() > limit {
                return Err(ApiError::new(
                    413,
                    "request_too_large",
                    format!("Request exceeds {limit} bytes"),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    let seconds = if limit == rdc_protocol::TRANSFER_CHUNK_BYTES {
        60
    } else {
        5
    };
    let deadline = worker::Delay::from(std::time::Duration::from_secs(seconds));
    match futures::future::select(Box::pin(receive), Box::pin(deadline)).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => Err(ApiError::new(
            408,
            "request_timeout",
            format!("Request body must arrive within {seconds} seconds"),
        )),
    }
}
pub async fn json_body<T: DeserializeOwned>(req: &mut Request) -> ApiResult<T> {
    Ok(serde_json::from_slice(
        &body(req, "application/json").await?,
    )?)
}

pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub async fn form(req: &mut Request) -> ApiResult<std::collections::BTreeMap<String, String>> {
    let bytes = body(req, "application/x-www-form-urlencoded").await?;
    let mut values = std::collections::BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(&bytes) {
        require(
            values
                .insert(key.into_owned(), value.into_owned())
                .is_none(),
            "Duplicate form parameter",
        )?;
        require(values.len() <= 24, "Too many parameters")?;
    }
    Ok(values)
}

pub fn param<'a>(
    map: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> ApiResult<&'a str> {
    map.get(name)
        .map(String::as_str)
        .ok_or_else(|| ApiError::bad(format!("Missing {name}")))
}

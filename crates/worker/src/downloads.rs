//! Capability-scoped binary transfer endpoints. Only metadata lives in Cloudflare storage.
use crate::{
    Commander,
    common::*,
    config::Config,
    crypto::random,
    relay::RelayClient,
    storage::{Device, Expiring},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rdc_protocol::{MAX_TRANSFER_BYTES, TRANSFER_CHUNK_BYTES, TRANSFER_TTL_SECONDS, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use worker::{Method, Request, Response};

#[derive(Serialize, Deserialize)]
struct TransferMeta {
    device_id: String,
    transfer_id: String,
    path: String,
    name: String,
    size: u64,
    upload: bool,
}
fn key(token: &str) -> String {
    format!("e:file-transfer:{token}")
}
fn transfer_path(path: &str) -> Option<(bool, &str, &str)> {
    let (upload, tail) = if let Some(tail) = path.strip_prefix("/upload/") {
        (true, tail)
    } else {
        (false, path.strip_prefix("/download/")?)
    };
    let (token, operation) = tail.split_once('/').unwrap_or((tail, ""));
    if token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some((upload, token, operation))
}
pub fn is_transfer_request(req: &Request) -> bool {
    let path = req.path();
    if req.method() == Method::Get && matches!(path.as_str(), "/transfer.js" | "/transfer.css") {
        return true;
    }
    let Some((upload, _, operation)) = transfer_path(&path) else {
        return false;
    };
    matches!(
        (upload, operation, req.method()),
        (_, "", Method::Get | Method::Delete)
            | (false, "", Method::Head)
            | (true, "status", Method::Get)
            | (true, "chunk" | "complete", Method::Post)
    )
}
pub fn is_upload_chunk(req: &Request) -> bool {
    req.method() == Method::Post
        && transfer_path(&req.path()).is_some_and(|(upload, _, op)| upload && op == "chunk")
}
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("download")
        .to_owned()
}
fn header_file_name(name: &str) -> String {
    let value: String = name
        .chars()
        .take(160)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if value.is_empty() {
        "download".into()
    } else {
        value
    }
}
fn mime_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" | "log" => "text/plain; charset=utf-8",
        "json" | "jsonl" => "application/json",
        "csv" => "text/csv; charset=utf-8",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => "application/octet-stream",
    }
}
fn agent_value(result: ToolResult) -> ApiResult<Value> {
    if result.is_error {
        let value = result.structured_content.unwrap_or(Value::Null);
        let code = value["error"].as_str().unwrap_or_default();
        let status = match code {
            "busy" => 429,
            "device_offline"
            | "connection_ended"
            | "device_disconnected"
            | "unknown_execution_state" => 503,
            _ => 409,
        };
        return Err(ApiError::new(
            status,
            "transfer_failed",
            value["message"]
                .as_str()
                .unwrap_or("Transfer failed. Inspect status before retrying."),
        ));
    }
    result.structured_content.ok_or_else(|| {
        ApiError::new(
            502,
            "invalid_agent_response",
            "Agent returned no transfer metadata",
        )
    })
}
async fn chunk(
    client: &RelayClient,
    id: &str,
    offset: u64,
    length: usize,
    size: u64,
) -> ApiResult<Vec<u8>> {
    let result = agent_value(
        client
            .call(
                "transfer_download_chunk",
                json!({"transfer_id":id,"offset":offset,"length":length}),
            )
            .await?,
    )?;
    let data = result["data"].as_str().ok_or_else(ApiError::internal)?;
    require(
        data.len() <= TRANSFER_CHUNK_BYTES.div_ceil(3) * 4,
        "Agent chunk exceeds limit",
    )?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| ApiError::new(502, "invalid_agent_response", "Malformed binary chunk"))?;
    if result["offset"] != offset
        || result["size"] != size
        || result["bytes"] != bytes.len()
        || bytes.len() != (size - offset).min(length as u64) as usize
    {
        return Err(ApiError::new(
            502,
            "invalid_agent_response",
            "Inconsistent binary chunk",
        ));
    }
    Ok(bytes)
}
// Inclusive single range. Suffix, open-ended and normal ranges are supported.
fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((size.saturating_sub(suffix), size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && end >= start).then_some((start, end))
}

impl Commander {
    pub async fn create_transfer(
        &self,
        device_id: &str,
        name: &str,
        arguments: Value,
        config: &Config,
    ) -> ApiResult<ToolResult> {
        let upload = name == "upload_file";
        let path = arguments["path"].as_str().unwrap_or_default().to_owned();
        let token = random()?;
        let started = self
            .relay(
                device_id,
                if upload {
                    "transfer_upload_begin"
                } else {
                    "transfer_download_begin"
                },
                arguments,
            )
            .await?;
        if started.is_error {
            return Ok(started);
        }
        let value = agent_value(started)?;
        let transfer_id = value["transfer_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?
            .to_owned();
        let size = value["size"]
            .as_u64()
            .filter(|size| *size <= MAX_TRANSFER_BYTES)
            .ok_or_else(ApiError::internal)?;
        let name = file_name(&path);
        let expires = now() + TRANSFER_TTL_SECONDS;
        let meta = TransferMeta {
            device_id: device_id.into(),
            transfer_id: transfer_id.clone(),
            path: path.clone(),
            name: name.clone(),
            size,
            upload,
        };
        let storage = self.state.storage();
        if let Err(error) = storage
            .put(
                &key(&token),
                Expiring {
                    expires,
                    data: meta,
                },
            )
            .await
        {
            let _ = self
                .relay(
                    device_id,
                    "transfer_cancel",
                    json!({"transfer_id":transfer_id}),
                )
                .await;
            return Err(error.into());
        }
        if storage.get_alarm().await?.is_none() {
            storage.set_alarm(Duration::from_secs(300)).await?;
        }
        let uri = format!(
            "{}/{}/{token}",
            config.origin,
            if upload { "upload" } else { "download" }
        );
        let metadata = json!({"path":path,"bytes":size,"max_transfer_bytes":MAX_TRANSFER_BYTES,"chunk_bytes":TRANSFER_CHUNK_BYTES,"expires_in":TRANSFER_TTL_SECONDS,"expires_at":expires,"device_must_remain_online":true,"cancel_url":uri});
        let mut metadata = metadata;
        if upload {
            metadata["upload_url"] = json!(uri);
            metadata["status_url"] = json!(format!("{uri}/status"));
            metadata["instructions"] = json!(
                "Open upload_url, choose a file of exactly bytes length, then start upload. HTTP clients may POST binary chunks to upload_url/chunk?offset=N, GET upload_url/status to resume, POST upload_url/complete to publish, or DELETE cancel_url to cancel. Links are private bearer capabilities; do not share them."
            );
            Ok(ToolResult::ok(metadata))
        } else {
            metadata["download_url"] = json!(uri);
            Ok(ToolResult::resource_link(
                uri,
                name.clone(),
                Some(mime_type(&name).into()),
                Some(size),
                metadata,
            ))
        }
    }

    pub async fn transfer_route(&self, req: &mut Request) -> ApiResult<Response> {
        let path = req.path();
        if path == "/transfer.js" {
            let mut response = Response::ok(include_str!("../../../web/transfer.js"))?;
            response
                .headers_mut()
                .set("Content-Type", "text/javascript; charset=utf-8")?;
            return Ok(response);
        }
        if path == "/transfer.css" {
            let mut response = Response::ok(include_str!("../../../web/transfer.css"))?;
            response
                .headers_mut()
                .set("Content-Type", "text/css; charset=utf-8")?;
            return Ok(response);
        }
        let (upload, token, operation) = transfer_path(&path)
            .ok_or_else(|| ApiError::new(404, "not_found", "Transfer not found"))?;
        let record = self
            .state
            .storage()
            .get::<Expiring<TransferMeta>>(&key(token))
            .await?
            .ok_or_else(|| ApiError::new(404, "not_found", "Transfer not found"))?;
        if record.expires <= now() {
            return Err(ApiError::new(410, "expired", "Transfer link expired"));
        }
        let meta = record.data;
        if meta.upload != upload
            || self
                .state
                .storage()
                .get::<Device>(&format!("p:device:{}", meta.device_id))
                .await?
                .is_none()
        {
            return Err(ApiError::new(
                404,
                "not_found",
                "Transfer not found or device revoked",
            ));
        }
        if req.method() == Method::Delete {
            self.state.storage().delete(&key(token)).await?;
            let result = self
                .relay(
                    &meta.device_id,
                    "transfer_cancel",
                    json!({"transfer_id":meta.transfer_id}),
                )
                .await;
            return json(
                &json!({"cancelled":true,"agent_notified":result.is_ok_and(|r| !r.is_error)}),
            );
        }
        if !upload {
            return self.stream_download(req, meta, record.expires).await;
        }
        if operation.is_empty() {
            return html(include_str!("../../../web/upload.html"));
        }
        let result = match operation {
            "status" => {
                self.relay(
                    &meta.device_id,
                    "transfer_status",
                    json!({"transfer_id":meta.transfer_id}),
                )
                .await?
            }
            "chunk" => {
                let content_type = req.headers().get("Content-Type")?.unwrap_or_default();
                require(
                    content_type.split(';').next().unwrap_or_default().trim()
                        == "application/octet-stream",
                    "Upload chunks require application/octet-stream",
                )?;
                let url = req.url()?;
                let offsets: Vec<_> = url
                    .query_pairs()
                    .filter(|(k, _)| k == "offset")
                    .map(|(_, v)| v.into_owned())
                    .collect();
                require(offsets.len() == 1, "Exactly one offset is required")?;
                let offset = offsets[0]
                    .parse::<u64>()
                    .map_err(|_| ApiError::bad("Invalid offset"))?;
                let bytes = bounded_bytes_limit(req, TRANSFER_CHUNK_BYTES).await?;
                require(!bytes.is_empty(), "Empty upload chunks are not accepted")?;
                require(
                    offset
                        .checked_add(bytes.len() as u64)
                        .is_some_and(|end| end <= meta.size && end <= MAX_TRANSFER_BYTES),
                    "Chunk exceeds the declared file size",
                )?;
                self.relay(&meta.device_id, "transfer_upload_chunk", json!({"transfer_id":meta.transfer_id,"offset":offset,"data":STANDARD.encode(bytes)})).await?
            }
            "complete" => {
                self.relay(
                    &meta.device_id,
                    "transfer_upload_complete",
                    json!({"transfer_id":meta.transfer_id}),
                )
                .await?
            }
            _ => {
                return Err(ApiError::new(
                    404,
                    "not_found",
                    "Transfer operation not found",
                ));
            }
        };
        let mut value = agent_value(result)?;
        value["chunk_bytes"] = json!(TRANSFER_CHUNK_BYTES);
        value["expires_at"] = json!(record.expires);
        json(&value)
    }

    async fn stream_download(
        &self,
        req: &Request,
        meta: TransferMeta,
        expires: u64,
    ) -> ApiResult<Response> {
        let etag = format!("\"rc-{}\"", meta.transfer_id);
        let range = req.headers().get("Range")?;
        let use_range = range.is_some()
            && req
                .headers()
                .get("If-Range")?
                .is_none_or(|value| value == etag);
        let (start, end) = if use_range {
            match byte_range(range.as_deref().unwrap_or_default(), meta.size) {
                Some(range) => range,
                None => {
                    let mut response = empty(416)?;
                    response
                        .headers_mut()
                        .set("Content-Range", &format!("bytes */{}", meta.size))?;
                    return Ok(response);
                }
            }
        } else {
            (0, meta.size.saturating_sub(1))
        };
        let length = if meta.size == 0 { 0 } else { end - start + 1 };
        let client = self.relay_client(&meta.device_id)?;
        let mut response = if req.method() == Method::Head {
            agent_value(
                client
                    .call("transfer_status", json!({"transfer_id":meta.transfer_id}))
                    .await?,
            )?;
            Response::empty()?
        } else {
            // Read and validate the first chunk before sending successful HTTP headers.
            let first = chunk(
                &client,
                &meta.transfer_id,
                start,
                (length.min(TRANSFER_CHUNK_BYTES as u64) as usize).max(1),
                meta.size,
            )
            .await?;
            let id = meta.transfer_id.clone();
            let size = meta.size;
            let stop = start + length;
            let stream = futures::stream::try_unfold(
                (client, id, start, Some(first)),
                move |(client, id, offset, first)| async move {
                    if offset >= stop {
                        return Ok(None);
                    }
                    if now() >= expires {
                        return Err(worker::Error::RustError(
                            "Transfer link expired; request a new link".into(),
                        ));
                    }
                    let bytes = if let Some(first) = first {
                        first
                    } else {
                        chunk(
                            &client,
                            &id,
                            offset,
                            (stop - offset).min(TRANSFER_CHUNK_BYTES as u64) as usize,
                            size,
                        )
                        .await
                        .map_err(|error| worker::Error::RustError(error.message))?
                    };
                    let next = offset + bytes.len() as u64;
                    Ok(Some((bytes, (client, id, next, None))))
                },
            );
            // Cloudflare derives Content-Length from the native fixed-length stream.
            let fixed: worker::worker_sys::FixedLengthStream =
                worker::FixedLengthStream::wrap(stream, length).into();
            Response::from_body(worker::ResponseBody::Stream(fixed.readable()))?
        };
        response = response.with_status(if use_range { 206 } else { 200 });
        let headers = response.headers_mut();
        headers.set("Content-Type", mime_type(&meta.name))?;
        let encoded: String = url::form_urlencoded::byte_serialize(meta.name.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        headers.set(
            "Content-Disposition",
            &format!(
                "attachment; filename=\"{}\"; filename*=UTF-8''{}",
                header_file_name(&meta.name),
                encoded
            ),
        )?;
        headers.set("Content-Length", &length.to_string())?;
        headers.set("Accept-Ranges", "bytes")?;
        headers.set("ETag", &etag)?;
        if use_range {
            headers.set(
                "Content-Range",
                &format!("bytes {start}-{end}/{}", meta.size),
            )?;
        }
        Ok(response)
    }
}

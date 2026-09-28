use crate::{
    Commander,
    common::*,
    config::Config,
    storage::{Audit, Device},
};
use rdc_protocol::{ToolResult, scope_for, tool_definitions};
use serde_json::{Value, json};
use worker::{Method, Request, Response};

const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

fn rpc_error(id: Value, code: i32, message: &str) -> ApiResult<Response> {
    json(&json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}))
}

fn validates(schema: &Value, value: &Value) -> bool {
    let valid_type = match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("integer") => value.as_u64().is_some(),
        Some("boolean") => value.is_boolean(),
        _ => false,
    };
    if !valid_type {
        return false;
    }
    if let Some(options) = schema["enum"].as_array()
        && !options.contains(value)
    {
        return false;
    }
    if let Some(number) = value.as_u64()
        && (schema["minimum"].as_u64().is_some_and(|x| number < x)
            || schema["maximum"].as_u64().is_some_and(|x| number > x))
    {
        return false;
    }
    if let Some(object) = value.as_object() {
        if schema["required"].as_array().is_some_and(|required| {
            required
                .iter()
                .any(|key| !object.contains_key(key.as_str().unwrap_or("")))
        }) {
            return false;
        }
        for (key, value) in object {
            match schema["properties"].get(key) {
                Some(property) if validates(property, value) => {}
                _ => return false,
            }
        }
    }
    if let Some(array) = value.as_array() {
        if schema["minItems"]
            .as_u64()
            .is_some_and(|n| array.len() < n as usize)
            || schema["maxItems"]
                .as_u64()
                .is_some_and(|n| array.len() > n as usize)
        {
            return false;
        }
        if array.iter().any(|item| !validates(&schema["items"], item)) {
            return false;
        }
    }
    true
}

impl Commander {
    pub async fn mcp(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        let grant = {
            let _guard = self.gate.lock().await;
            let grant = self.authenticate(req, config).await?;
            self.rate("mcp", 120).await?;
            grant
        };
        if req.method() != Method::Post {
            let mut response = empty(405)?;
            response.headers_mut().set("Allow", "POST")?;
            return Ok(response);
        }
        if let Some(version) = req.headers().get("MCP-Protocol-Version")? {
            require(
                VERSIONS.contains(&version.as_str()),
                "Unsupported MCP protocol version",
            )?;
        }
        let accept = req.headers().get("Accept")?.unwrap_or_default();
        require(
            accept.contains("application/json") && accept.contains("text/event-stream"),
            "Accept must include application/json and text/event-stream",
        )?;
        let bytes = body(req, "application/json").await?;
        let request: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return rpc_error(Value::Null, -32700, "Parse error"),
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if !request.is_object()
            || request["jsonrpc"] != "2.0"
            || !request["method"].is_string()
            || (request.get("id").is_some() && !(id.is_string() || id.is_i64() || id.is_u64()))
        {
            return rpc_error(Value::Null, -32600, "Invalid Request");
        }
        let method = request["method"].as_str().unwrap();
        if request.get("id").is_none() {
            // Notifications never execute tools. This service has no resumable
            // SSE sessions; process cancellation is an explicit tool operation.
            return empty(202);
        }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return rpc_error(id, -32602, "params must be an object");
        }
        let result = match method {
            "initialize" => {
                let Some(version) = params["protocolVersion"].as_str() else {
                    return rpc_error(id, -32602, "protocolVersion is required");
                };
                if !params["capabilities"].is_object() || !params["clientInfo"].is_object() {
                    return rpc_error(id, -32602, "clientInfo and capabilities are required");
                }
                json!({"protocolVersion":if VERSIONS.contains(&version) { version } else { rdc_protocol::PROTOCOL_VERSION },"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"remote-commander","version":env!("CARGO_PKG_VERSION")},"instructions":"Use list_devices to select a paired computer. Paths are relative to its configured root. For binary or large files up to 1 GiB, use download_file or upload_file rather than text read/write tools. Return the private transfer URL to the user; never embed file bytes in tool arguments. Uploads require exact size and local --allow-write. Transfer links expire after one hour; keep the device online. Shell access also requires local permission. After a lost reply, inspect state before repeating a change."})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools":tool_definitions()}),
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return rpc_error(id, -32602, "Tool name is required");
                };
                let Some(scope) = scope_for(name) else {
                    return rpc_error(id, -32602, "Unknown tool");
                };
                if !grant.scope.split_whitespace().any(|s| s == scope) {
                    let mut result = serde_json::to_value(ToolResult::error(
                        "insufficient_scope",
                        format!("This tool requires {scope}"),
                    ))?;
                    result["_meta"] = json!({"mcp/www_authenticate":[format!("{}, scope=\"{}\"",config.challenge("insufficient_scope"),scope)]});
                    return json(&json!({"jsonrpc":"2.0","id":id,"result":result}));
                }
                let mut arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let schema = tool_definitions()
                    .into_iter()
                    .find(|t| t["name"] == name)
                    .unwrap();
                if !validates(&schema["inputSchema"], &arguments) {
                    return rpc_error(id, -32602, "Arguments do not match the tool schema");
                }
                let device_id = arguments
                    .get("device_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let result = match name {
                    "list_devices" => ToolResult::ok(self.devices().await?),
                    "get_recent_activity" => {
                        let activity: Vec<Audit> = self
                            .state
                            .storage()
                            .get("p:audit")
                            .await?
                            .unwrap_or_default();
                        ToolResult::ok(json!({"activity":activity}))
                    }
                    _ => {
                        let device_id = device_id.as_deref().unwrap_or_default();
                        let device: Option<Device> = self
                            .state
                            .storage()
                            .get(&format!("p:device:{device_id}"))
                            .await?;
                        if let Some(device) = device {
                            if name == "get_device_info" {
                                ToolResult::ok(self.public_device(&device))
                            } else {
                                arguments.as_object_mut().unwrap().remove("device_id");
                                if name == "download_file" || name == "upload_file" {
                                    self.create_transfer(device_id, name, arguments, config)
                                        .await?
                                } else {
                                    self.relay(device_id, name, arguments).await?
                                }
                            }
                        } else {
                            ToolResult::error(
                                "device_not_found",
                                "Pair a device and use its exact identifier from list_devices",
                            )
                        }
                    }
                };
                {
                    let _guard = self.gate.lock().await;
                    self.audit(
                        name,
                        device_id.as_deref(),
                        if result.is_error { "error" } else { "success" },
                    )
                    .await?;
                }
                serde_json::to_value(result)?
            }
            _ => return rpc_error(id, -32601, "Method not found"),
        };
        json(&json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

use crate::{Commander, common::*, config::bearer, crypto::*, storage::Device};
use futures::{
    channel::oneshot,
    future::{Either, select},
};
use rdc_protocol::{AgentResponse, Command, MAX_FRAME_BYTES, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};
use worker::{Delay, Request, Response, WebSocket, WebSocketIncomingMessage, WebSocketPair};

#[derive(Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub device_id: String,
    pub generation: String,
    pub active: bool,
}

pub struct Pending {
    pub device_id: String,
    pub generation: String,
    pub reply: oneshot::Sender<ToolResult>,
}

impl Commander {
    pub fn public_device(&self, device: &Device) -> Value {
        let online = self
            .state
            .get_websockets_with_tag(&device.id)
            .iter()
            .any(|socket| {
                socket
                    .deserialize_attachment::<Attachment>()
                    .ok()
                    .flatten()
                    .is_some_and(|a| a.active)
            });
        json!({"device_id":device.id,"name":device.name,"paired_at":device.paired_at,"online":online})
    }

    pub fn fail_pending(&self, attachment: &Attachment, code: &str) {
        let mut pending = self.pending.borrow_mut();
        let ids: Vec<_> = pending
            .iter()
            .filter(|(_, p)| {
                p.device_id == attachment.device_id && p.generation == attachment.generation
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(p) = pending.remove(&id) {
                let _ = p.reply.send(ToolResult::error(code,"Connection ended before a result was received. The operation may have completed; do not blindly retry changes."));
            }
        }
    }

    pub fn disconnect(&self, id: &str, code: u16, reason: &str) {
        for socket in self.state.get_websockets_with_tag(id) {
            if let Ok(Some(mut attachment)) = socket.deserialize_attachment::<Attachment>() {
                attachment.active = false;
                let _ = socket.serialize_attachment(&attachment);
                self.fail_pending(&attachment, "connection_ended");
            }
            let _ = socket.close(Some(code), Some(reason));
        }
    }

    pub async fn connect_agent(&self, req: &Request) -> ApiResult<Response> {
        let path = req.path();
        let id = path.trim_start_matches("/agent/");
        require(
            id.len() == 43
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid device identifier",
        )?;
        let token = bearer(req)?;
        let device: Device = self
            .state
            .storage()
            .get(&format!("p:device:{id}"))
            .await?
            .ok_or_else(|| ApiError::new(401, "invalid_token", "Device not paired"))?;
        if !secret_equal(&device.token_hash, &hash(&token)) {
            return Err(ApiError::new(401, "invalid_token", "Device token rejected"));
        }
        require(
            req.headers()
                .get("Upgrade")?
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket")),
            "WebSocket upgrade required",
        )?;
        let pair = WebSocketPair::new()?;
        self.disconnect(id, 4001, "New connection replaced this session");
        self.state.accept_websocket_with_tags(&pair.server, &[id]);
        pair.server.serialize_attachment(Attachment {
            device_id: id.into(),
            generation: random()?,
            active: true,
        })?;
        Ok(Response::from_websocket(pair.client)?)
    }

    pub fn relay_client(&self, device_id: &str) -> ApiResult<RelayClient> {
        let selected = self
            .state
            .get_websockets_with_tag(device_id)
            .into_iter()
            .find_map(|socket| {
                let attachment = socket
                    .deserialize_attachment::<Attachment>()
                    .ok()
                    .flatten()?;
                if attachment.active {
                    Some((socket, attachment))
                } else {
                    None
                }
            });
        let (socket, attachment) = selected
            .ok_or_else(|| ApiError::new(503, "device_offline", "Device agent is not connected"))?;
        Ok(RelayClient {
            socket,
            attachment,
            pending: self.pending.clone(),
        })
    }

    pub async fn relay(
        &self,
        device_id: &str,
        name: &str,
        arguments: Value,
    ) -> ApiResult<ToolResult> {
        match self.relay_client(device_id) {
            Ok(client) => client.call(name, arguments).await,
            Err(error) => Ok(ToolResult::error(error.code, error.message)),
        }
    }

    pub fn receive_agent(
        &self,
        socket: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> worker::Result<()> {
        let Some(attachment) = socket.deserialize_attachment::<Attachment>()? else {
            return Ok(());
        };
        if !attachment.active {
            return Ok(());
        }
        let WebSocketIncomingMessage::String(text) = message else {
            self.end_socket(&socket, "invalid_agent_message");
            return Ok(());
        };
        if text.len() > MAX_FRAME_BYTES {
            self.end_socket(&socket, "agent_output_limit");
            return Ok(());
        }
        let response: AgentResponse = match serde_json::from_str(&text) {
            Ok(response) => response,
            Err(_) => {
                self.end_socket(&socket, "invalid_agent_message");
                return Ok(());
            }
        };
        let mut pending = self.pending.borrow_mut();
        let matches = pending.get(&response.id).is_some_and(|p| {
            p.device_id == attachment.device_id && p.generation == attachment.generation
        });
        if matches && let Some(p) = pending.remove(&response.id) {
            let _ = p.reply.send(response.result);
        }
        Ok(())
    }

    pub fn end_socket(&self, socket: &WebSocket, code: &str) {
        if let Ok(Some(mut attachment)) = socket.deserialize_attachment::<Attachment>() {
            attachment.active = false;
            let _ = socket.serialize_attachment(&attachment);
            self.fail_pending(&attachment, code);
        }
        let _ = socket.close(Some(4002), Some("Connection ended"));
    }
}

// Owned by a streaming response; no entire-file buffers or per-chunk HTTP subrequests.
#[derive(Clone)]
pub struct RelayClient {
    socket: WebSocket,
    attachment: Attachment,
    pending: Rc<RefCell<HashMap<String, Pending>>>,
}
struct PendingGuard {
    id: String,
    pending: Rc<RefCell<HashMap<String, Pending>>>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.borrow_mut().remove(&self.id);
    }
}
impl RelayClient {
    pub async fn call(&self, name: &str, arguments: Value) -> ApiResult<ToolResult> {
        if self.pending.borrow().len() >= 8 {
            return Ok(ToolResult::error(
                "busy",
                "At most eight tool calls may be in flight. No command was dispatched.",
            ));
        }
        if !self
            .socket
            .deserialize_attachment::<Attachment>()?
            .is_some_and(|a| a.active && a.generation == self.attachment.generation)
        {
            return Ok(ToolResult::error(
                "device_offline",
                "Device connection changed. Resume with a new HTTP request.",
            ));
        }
        let id = random()?;
        let command = Command {
            id: id.clone(),
            name: name.into(),
            arguments,
        };
        let (reply, response) = oneshot::channel();
        self.pending.borrow_mut().insert(
            id.clone(),
            Pending {
                device_id: self.attachment.device_id.clone(),
                generation: self.attachment.generation.clone(),
                reply,
            },
        );
        // Remove the request even if a client cancels a partially read response.
        let _guard = PendingGuard {
            id,
            pending: self.pending.clone(),
        };
        if self.socket.send(&command).is_err() {
            return Ok(ToolResult::error(
                "dispatch_failed",
                "Delivery failed. Inspect transfer status before retrying a change.",
            ));
        }
        let timeout = Box::pin(Delay::from(Duration::from_secs(25)));
        let result = match select(response, timeout).await {
            Either::Left((Ok(result), _)) => result,
            _ => ToolResult::error(
                "unknown_execution_state",
                "No result arrived within 25 seconds. The command may have executed. Inspect state before retrying a change.",
            ),
        };
        Ok(result)
    }
}

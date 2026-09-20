use crate::{
    Commander,
    common::*,
    config::Config,
    crypto::*,
    storage::{Audit, Device, Expiring, Pairing},
};
use serde::Deserialize;
use serde_json::json;
use worker::{Method, Request, Response};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Redeem {
    device_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approve {
    user_code: String,
}

fn normalize_code(code: &str) -> ApiResult<String> {
    let code = code.trim().to_ascii_uppercase();
    require(
        code.len() == 8 && code.bytes().all(|b| b.is_ascii_alphanumeric()),
        "Expected an eight-character pairing code",
    )?;
    Ok(code)
}

impl Commander {
    pub async fn start_pairing(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        self.rate("pair-start", 20).await?;
        let args: Start = json_body(req).await?;
        require(
            !args.name.trim().is_empty() && args.name.len() <= 80,
            "Device name must contain 1–80 bytes",
        )?;
        let pending = self
            .list::<Expiring<Pairing>>("e:pair:", 64)
            .await?
            .into_iter()
            .filter(|(_, r)| r.expires > now())
            .count();
        require(pending < 32, "Too many pending pairing requests")?;
        let device_code = random()?;
        let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let user_code: String = random()?
            .bytes()
            .take(8)
            .map(|b| alphabet[(b as usize) % alphabet.len()] as char)
            .collect();
        let code_key = format!("e:pair-code:{}", hash(&user_code));
        if self.get::<String>(&code_key).await?.is_some() {
            return Err(ApiError::new(
                409,
                "code_collision",
                "Start a new pairing request",
            ));
        }
        let device_hash = hash(&device_code);
        self.put(
            &format!("e:pair:{device_hash}"),
            Pairing {
                name: args.name,
                user_code: user_code.clone(),
                approved: false,
                last_poll: 0,
            },
            600,
        )
        .await?;
        self.put(&code_key, &device_hash, 600).await?;
        json(
            &json!({"device_code":device_code,"user_code":user_code,"verification_uri":config.origin,"verification_uri_complete":format!("{}/?pair={user_code}",config.origin),"expires_in":600,"interval":2}),
        )
    }

    pub async fn redeem_pairing(&self, req: &mut Request) -> ApiResult<Response> {
        self.rate("pair-token", 120).await?;
        let args: Redeem = json_body(req).await?;
        require(args.device_code.len() == 43, "Invalid device_code")?;
        let key = format!("e:pair:{}", hash(&args.device_code));
        let mut record = self
            .state
            .storage()
            .get::<Expiring<Pairing>>(&key)
            .await?
            .filter(|r| r.expires > now())
            .ok_or_else(|| {
                ApiError::new(400, "expired_token", "Pairing expired or already redeemed")
            })?;
        if now().saturating_sub(record.data.last_poll) < 2 {
            return Err(ApiError::new(
                400,
                "slow_down",
                "Poll no more than once every two seconds",
            ));
        }
        if !record.data.approved {
            record.data.last_poll = now();
            self.state.storage().put(&key, &record).await?;
            return Err(ApiError::new(
                400,
                "authorization_pending",
                "Waiting for owner approval",
            ));
        }
        require(
            self.list::<Device>("p:device:", 65).await?.len() < 64,
            "Device capacity reached",
        )?;
        // Consume first: a crash fails closed and requires a fresh pairing, never
        // a second redemption of a previously authorized code.
        self.state.storage().delete(&key).await?;
        self.state
            .storage()
            .delete(&format!("e:pair-code:{}", hash(&record.data.user_code)))
            .await?;
        let token = random()?;
        let id = random()?;
        let device = Device {
            id: id.clone(),
            name: record.data.name,
            token_hash: hash(&token),
            paired_at: now(),
        };
        self.state
            .storage()
            .put(&format!("p:device:{id}"), device)
            .await?;
        self.audit("device_paired", Some(&id), "success").await?;
        json(&json!({"device_id":id,"token":token}))
    }

    pub async fn devices(&self) -> ApiResult<serde_json::Value> {
        let devices = self.list::<Device>("p:device:", 64).await?;
        Ok(
            json!({"devices":devices.into_iter().map(|(_,d)|self.public_device(&d)).collect::<Vec<_>>()}),
        )
    }

    pub async fn admin_route(&self, req: &mut Request, config: &Config) -> ApiResult<Response> {
        if config.check_admin(req).is_err() {
            self.rate("admin-invalid", 30).await?;
            return Err(ApiError::new(
                401,
                "unauthorized",
                "Administrator credential required",
            ));
        }
        let path = req.path();
        match (req.method(), path.as_str()) {
            (Method::Get, "/api/connection") => self.connection_status(config).await,
            (Method::Post, "/api/connection/open") => self.reset_connection(true).await,
            (Method::Get, "/api/devices") => json(&self.devices().await?),
            (Method::Get, "/api/activity") => {
                let activity: Vec<Audit> = self
                    .state
                    .storage()
                    .get("p:audit")
                    .await?
                    .unwrap_or_default();
                json(&json!({"activity":activity}))
            }
            (Method::Post, "/api/pair/approve") => {
                let args: Approve = json_body(req).await?;
                let code = normalize_code(&args.user_code)?;
                let device_hash = self
                    .get::<String>(&format!("e:pair-code:{}", hash(&code)))
                    .await?
                    .ok_or_else(|| ApiError::bad("Pairing code expired or not found"))?;
                let key = format!("e:pair:{device_hash}");
                let mut record = self
                    .state
                    .storage()
                    .get::<Expiring<Pairing>>(&key)
                    .await?
                    .filter(|r| r.expires > now())
                    .ok_or_else(|| ApiError::bad("Pairing expired"))?;
                record.data.approved = true;
                self.state.storage().put(&key, &record).await?;
                json(&json!({"approved":true,"name":record.data.name}))
            }
            (Method::Post, "/api/clients/revoke") => self.reset_connection(false).await,
            (Method::Get, _) if path.starts_with("/api/pair/") => {
                let code = normalize_code(path.trim_start_matches("/api/pair/"))?;
                let device_hash = self
                    .get::<String>(&format!("e:pair-code:{}", hash(&code)))
                    .await?
                    .ok_or_else(|| ApiError::bad("Pairing code expired or not found"))?;
                let record = self
                    .get::<Pairing>(&format!("e:pair:{device_hash}"))
                    .await?
                    .ok_or_else(|| ApiError::bad("Pairing expired"))?;
                json(
                    &json!({"name":record.name,"user_code":record.user_code,"approved":record.approved}),
                )
            }
            (Method::Delete, _) if path.starts_with("/api/devices/") => {
                let id = path.trim_start_matches("/api/devices/");
                require(
                    id.len() == 43
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                    "Invalid device ID",
                )?;
                self.state
                    .storage()
                    .delete(&format!("p:device:{id}"))
                    .await?;
                self.disconnect(id, 4003, "Device revoked");
                self.audit("device_revoked", Some(id), "success").await?;
                json(&json!({"revoked":true}))
            }
            _ => Err(ApiError::new(
                404,
                "not_found",
                "Unknown administration endpoint",
            )),
        }
    }
}

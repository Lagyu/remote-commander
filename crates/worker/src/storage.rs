use crate::{Commander, common::*};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::time::Duration;
use worker::ListOptions;

#[derive(Serialize, Deserialize)]
pub struct Expiring<T> {
    pub expires: u64,
    pub data: T,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub paired_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub name: String,
    pub user_code: String,
    pub approved: bool,
    pub last_poll: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Audit {
    pub at: u64,
    pub tool: String,
    pub device_id: Option<String>,
    pub status: String,
}

impl Commander {
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> ApiResult<Option<T>> {
        let record: Option<Expiring<T>> = self.state.storage().get(key).await?;
        match record {
            Some(record) if record.expires > now() => Ok(Some(record.data)),
            Some(_) => {
                self.state.storage().delete(key).await?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    pub async fn put<T: Serialize>(&self, key: &str, data: T, ttl: u64) -> ApiResult<()> {
        self.state
            .storage()
            .put(
                key,
                Expiring {
                    expires: now() + ttl,
                    data,
                },
            )
            .await?;
        if self.state.storage().get_alarm().await?.is_none() {
            self.state
                .storage()
                .set_alarm(Duration::from_secs(300))
                .await?;
        }
        Ok(())
    }

    pub async fn list<T: DeserializeOwned>(
        &self,
        prefix: &str,
        limit: usize,
    ) -> ApiResult<Vec<(String, T)>> {
        let map = self
            .state
            .storage()
            .list_with_options(ListOptions::new().prefix(prefix).limit(limit))
            .await?;
        let iterator = js_sys::try_iter(&map)
            .map_err(|_| ApiError::internal())?
            .ok_or_else(ApiError::internal)?;
        let mut records = Vec::new();
        for item in iterator {
            let pair = js_sys::Array::from(&item.map_err(|_| ApiError::internal())?);
            let key = pair.get(0).as_string().ok_or_else(ApiError::internal)?;
            let value =
                serde_wasm_bindgen::from_value(pair.get(1)).map_err(|_| ApiError::internal())?;
            records.push((key, value));
        }
        Ok(records)
    }

    // Callers hold gate across read/modify/write; no network wait holds that gate.
    pub async fn rate(&self, label: &str, maximum: u32) -> ApiResult<()> {
        let key = format!("e:rate:{label}:{}", now() / 60);
        let count = self.get::<u32>(&key).await?.unwrap_or(0);
        if count >= maximum {
            return Err(ApiError::new(
                429,
                "slow_down",
                "Request limit reached; retry after one minute",
            ));
        }
        self.put(&key, count + 1, 120).await
    }

    pub async fn audit(&self, tool: &str, device_id: Option<&str>, status: &str) -> ApiResult<()> {
        let mut records: Vec<Audit> = self
            .state
            .storage()
            .get("p:audit")
            .await?
            .unwrap_or_default();
        records.push(Audit {
            at: now(),
            tool: tool.into(),
            device_id: device_id.map(str::to_owned),
            status: status.into(),
        });
        if records.len() > 200 {
            records.drain(..records.len() - 200);
        }
        self.state.storage().put("p:audit", records).await?;
        Ok(())
    }

    pub async fn cleanup(&self) -> ApiResult<()> {
        let cursor: Option<String> = self.state.storage().get("p:cleanup_cursor").await?;
        let options = ListOptions::new().prefix("e:").limit(1000);
        let options = if let Some(cursor) = cursor.as_deref() {
            // The SDK exposes an inclusive cursor. Rechecking the boundary key
            // is harmless; a full page still advances by at least 999 keys.
            options.start(cursor)
        } else {
            options
        };
        let map = self.state.storage().list_with_options(options).await?;
        let iterator = js_sys::try_iter(&map)
            .map_err(|_| ApiError::internal())?
            .ok_or_else(ApiError::internal)?;
        let mut last = String::new();
        let mut count = 0;
        for item in iterator {
            let pair = js_sys::Array::from(&item.map_err(|_| ApiError::internal())?);
            let key = pair.get(0).as_string().ok_or_else(ApiError::internal)?;
            let record: Expiring<Value> =
                serde_wasm_bindgen::from_value(pair.get(1)).map_err(|_| ApiError::internal())?;
            if record.expires <= now() {
                self.state.storage().delete(&key).await?;
            }
            last = key;
            count += 1;
        }
        if count == 1000 {
            self.state.storage().put("p:cleanup_cursor", last).await?;
        } else {
            self.state.storage().delete("p:cleanup_cursor").await?;
        }
        self.state
            .storage()
            .set_alarm(Duration::from_secs(300))
            .await?;
        Ok(())
    }
}

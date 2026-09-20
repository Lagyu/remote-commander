use crate::common::{ApiError, ApiResult};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn random() -> ApiResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| ApiError::internal())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

pub fn secret_equal(left: &str, right: &str) -> bool {
    bool::from(Sha256::digest(left.as_bytes()).ct_eq(&Sha256::digest(right.as_bytes())))
}

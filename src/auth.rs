use sha2::{Sha256, Digest};
use rand::RngCore;
use axum::http::HeaderMap;
use crate::error::AppError;

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("naleys_{}", hex::encode(bytes))
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn extract_bearer(headers: &HeaderMap) -> Result<&str, AppError> {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    auth.strip_prefix("Bearer ").ok_or(AppError::Unauthorized)
}

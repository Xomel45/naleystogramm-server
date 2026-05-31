use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    Db(sqlx::Error),
    NotFound(&'static str),
    Conflict(&'static str),
    BadRequest(String),
    Forbidden(&'static str),
    Unauthorized,
    TooManyRequests,
    Internal(String),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self { AppError::Db(e) }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, detail): (StatusCode, &str, Option<String>) = match self {
            AppError::NotFound(e)      => (StatusCode::NOT_FOUND,            e, None),
            AppError::Conflict(e)      => (StatusCode::CONFLICT,             e, None),
            AppError::BadRequest(e)    => (StatusCode::BAD_REQUEST,  "bad_request", Some(e)),
            AppError::Forbidden(e)     => (StatusCode::FORBIDDEN,            e, None),
            AppError::Unauthorized     => (StatusCode::UNAUTHORIZED, "unauthorized", None),
            AppError::TooManyRequests  => (StatusCode::TOO_MANY_REQUESTS, "rate_limited", None),
            AppError::Db(e)            => (StatusCode::INTERNAL_SERVER_ERROR, "db_error", Some(e.to_string())),
            AppError::Internal(e)      => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", Some(e)),
        };
        let body = if let Some(d) = detail {
            json!({ "error": code, "detail": d })
        } else {
            json!({ "error": code })
        };
        (status, Json(body)).into_response()
    }
}

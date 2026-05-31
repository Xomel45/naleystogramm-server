use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::{
    auth::{extract_bearer, generate_token, hash_token},
    error::AppError,
    state::AppState,
    version::satisfies,
};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/info",              get(info))
        .route("/register",          post(register))
        .route("/lookup/{username}",  get(lookup))
        .route("/search",            get(search))
        .route("/update",            put(update))
        .route("/heartbeat",         post(heartbeat))
        .route("/unregister",        delete(unregister))
        .route("/token/refresh",     post(token_refresh))
        .route("/invite/create",     post(invite_create))
        .with_state(state)
}

// ── /info ────────────────────────────────────────────────────────────────────

async fn info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (user_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&state.pool)
            .await
            .unwrap_or((0,));
    let (online_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM presence WHERE last_seen > datetime('now', '-30 minutes')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0,));

    let min_ver = state.min_version.read().await.clone();
    Json(json!({
        "name":               state.config.server.name,
        "description":        state.config.server.description,
        "mode":               state.mode,
        "version":            env!("CARGO_PKG_VERSION"),
        "public":             state.config.server.public,
        "open":               state.config.registration.open,
        "require_email":      state.config.registration.require_email,
        "require_invite":     state.config.registration.require_invite,
        "min_client_version": min_ver,
        "relay_enabled":      state.config.relay.enabled,
        "stats": {
            "users_total":  user_count,
            "users_online": online_count,
        }
    }))
    .into_response()
}

// ── /register ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RegisterReq {
    username:       String,
    pubkey:         String,
    ip:             String,
    port:           u16,
    email:          Option<String>,
    invite_code:    Option<String>,
    client_version: Option<String>,
}

async fn register(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterReq>,
) -> Result<impl IntoResponse, AppError> {
    if !state.rate_limiter.allow_register(addr.ip()).await {
        return Err(AppError::TooManyRequests);
    }

    // min_client_version check
    if let Some(ref cv) = body.client_version {
        let min = state.min_version.read().await.clone();
        if min != "0.0.0" && !satisfies(cv, &min) {
            return Ok((
                StatusCode::UPGRADE_REQUIRED,
                Json(json!({ "error": "client_too_old", "min_version": min })),
            )
                .into_response());
        }
    }

    // Registration open / invite check
    if !state.config.registration.open {
        let Some(ref code) = body.invite_code else {
            return Err(AppError::Forbidden("registration_closed"));
        };
        let row: Option<(String,)> =
            sqlx::query_as("SELECT code FROM invites WHERE code=? AND used_by IS NULL")
                .bind(code)
                .fetch_optional(&state.pool)
                .await?;
        if row.is_none() {
            return Err(AppError::Forbidden("invalid_invite"));
        }
    }

    // Username validation
    let uname_len = body.username.chars().count();
    let cfg = &state.config.registration;
    if uname_len < cfg.username_min_length || uname_len > cfg.username_max_length {
        return Err(AppError::BadRequest(format!(
            "username length must be {}-{}",
            cfg.username_min_length, cfg.username_max_length
        )));
    }
    let re = Regex::new(&cfg.username_regex)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !re.is_match(&body.username) {
        return Err(AppError::BadRequest("invalid_username".into()));
    }

    // Username taken (case-insensitive)
    let username_lc = body.username.to_lowercase();
    let taken: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM users WHERE username_lc=?")
            .bind(&username_lc)
            .fetch_optional(&state.pool)
            .await?;
    if taken.is_some() {
        return Err(AppError::Conflict("username_taken"));
    }

    // Pubkey validation: base64, 32 bytes (X25519)
    let key_bytes = B64.decode(&body.pubkey)
        .map_err(|_| AppError::BadRequest("invalid pubkey: bad base64".into()))?;
    if key_bytes.len() != 32 {
        return Err(AppError::BadRequest("invalid pubkey: must be 32 bytes".into()));
    }

    // IP / port
    body.ip
        .parse::<std::net::IpAddr>()
        .map_err(|_| AppError::BadRequest("invalid ip".into()))?;

    // Email
    if state.config.registration.require_email {
        let email = body.email.as_deref().unwrap_or("");
        if email.is_empty() || !email.contains('@') {
            return Err(AppError::BadRequest("email required".into()));
        }
    }

    // Max users
    if state.config.registration.max_users > 0 {
        let (cnt,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&state.pool)
            .await?;
        if cnt >= state.config.registration.max_users as i64 {
            return Ok((
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "server_full" })),
            )
                .into_response());
        }
    }

    // Insert user
    let result = sqlx::query(
        "INSERT INTO users (username, username_lc, pubkey, email) VALUES (?,?,?,?)",
    )
    .bind(&body.username)
    .bind(&username_lc)
    .bind(&body.pubkey)
    .bind(&body.email)
    .execute(&state.pool)
    .await?;

    let user_id = result.last_insert_rowid();

    // Insert presence
    sqlx::query("INSERT INTO presence (user_id, ip, port) VALUES (?,?,?)")
        .bind(user_id)
        .bind(&body.ip)
        .bind(body.port as i64)
        .execute(&state.pool)
        .await?;

    // Generate token
    let token = generate_token();
    let token_hash = hash_token(&token);
    let expires_at: Option<String> = if state.config.tokens.ttl_days > 0 {
        Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(state.config.tokens.ttl_days as i64))
                .unwrap()
                .to_rfc3339(),
        )
    } else {
        None
    };
    sqlx::query("INSERT INTO tokens (user_id, token_hash, expires_at) VALUES (?,?,?)")
        .bind(user_id)
        .bind(&token_hash)
        .bind(&expires_at)
        .execute(&state.pool)
        .await?;

    // Mark invite as used
    if let Some(ref code) = body.invite_code {
        sqlx::query("UPDATE invites SET used_by=?, used_at=datetime('now') WHERE code=?")
            .bind(user_id)
            .bind(code)
            .execute(&state.pool)
            .await?;
    }

    let host = format!("{}:{}", state.config.server.name, state.config.server.port);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "token": token, "username": body.username, "server": host })),
    )
        .into_response())
}

// ── /lookup/:username ─────────────────────────────────────────────────────────

async fn lookup(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if !state.rate_limiter.allow_lookup(addr.ip()).await {
        return Err(AppError::TooManyRequests);
    }

    let row: Option<(String, String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT u.pubkey, p.ip, p.port, p.last_seen, u.username
         FROM users u JOIN presence p ON u.id=p.user_id
         WHERE u.username_lc=?",
    )
    .bind(username.to_lowercase())
    .fetch_optional(&state.pool)
    .await?;

    let Some((pubkey, ip, port, last_seen, uname)) = row else {
        return Err(AppError::NotFound("not_found"));
    };

    let offline_mins = state.config.presence.offline_after_minutes as i64;
    let online: bool = sqlx::query_as::<_, (bool,)>(
        "SELECT last_seen > datetime('now', ? || ' minutes') FROM presence WHERE ip=?",
    )
    .bind(format!("-{offline_mins}"))
    .bind(&ip)
    .fetch_one(&state.pool)
    .await
    .map(|(v,)| v)
    .unwrap_or(false);

    Ok(Json(json!({
        "username":  uname,
        "pubkey":    pubkey,
        "ip":        ip,
        "port":      port,
        "online":    online,
        "last_seen": last_seen,
    }))
    .into_response())
}

// ── /search ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    let q = params.q.unwrap_or_default();
    if q.is_empty() {
        return Ok(Json(json!({ "users": [] })).into_response());
    }
    let prefix = format!("{}%", q.to_lowercase());
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT username FROM users WHERE username_lc LIKE ? LIMIT 20")
            .bind(&prefix)
            .fetch_all(&state.pool)
            .await?;
    let names: Vec<&str> = rows.iter().map(|(n,)| n.as_str()).collect();
    Ok(Json(json!({ "users": names })).into_response())
}

// ── /update ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UpdateReq {
    ip:   String,
    port: u16,
}

async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UpdateReq>,
) -> Result<impl IntoResponse, AppError> {
    let user_id = resolve_token(&state, &headers).await?;
    body.ip
        .parse::<std::net::IpAddr>()
        .map_err(|_| AppError::BadRequest("invalid ip".into()))?;

    sqlx::query(
        "INSERT INTO presence (user_id, ip, port, last_seen)
         VALUES (?,?,?, datetime('now'))
         ON CONFLICT(user_id) DO UPDATE SET ip=excluded.ip, port=excluded.port, last_seen=datetime('now')",
    )
    .bind(user_id)
    .bind(&body.ip)
    .bind(body.port as i64)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({ "ok": true })).into_response())
}

// ── /heartbeat ────────────────────────────────────────────────────────────────

async fn heartbeat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user_id = resolve_token(&state, &headers).await?;
    sqlx::query("UPDATE presence SET last_seen=datetime('now') WHERE user_id=?")
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

// ── /unregister ───────────────────────────────────────────────────────────────

async fn unregister(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user_id = resolve_token(&state, &headers).await?;
    sqlx::query("DELETE FROM users WHERE id=?")
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    Ok((StatusCode::NO_CONTENT, ()).into_response())
}

// ── /token/refresh ─────────────────────────────────────────────────────────────

async fn token_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user_id = resolve_token(&state, &headers).await?;

    // Count existing tokens
    let (cnt,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tokens WHERE user_id=?")
            .bind(user_id)
            .fetch_one(&state.pool)
            .await?;
    if cnt >= state.config.tokens.max_per_user as i64 {
        // Delete oldest
        sqlx::query(
            "DELETE FROM tokens WHERE id=(SELECT id FROM tokens WHERE user_id=? ORDER BY created_at LIMIT 1)",
        )
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    }

    let token = generate_token();
    let token_hash = hash_token(&token);
    let expires_at: Option<String> = if state.config.tokens.ttl_days > 0 {
        Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(state.config.tokens.ttl_days as i64))
                .unwrap()
                .to_rfc3339(),
        )
    } else {
        None
    };
    sqlx::query("INSERT INTO tokens (user_id, token_hash, expires_at) VALUES (?,?,?)")
        .bind(user_id)
        .bind(&token_hash)
        .bind(&expires_at)
        .execute(&state.pool)
        .await?;

    Ok(Json(json!({ "token": token })).into_response())
}

// ── /invite/create ────────────────────────────────────────────────────────────

async fn invite_create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user_id = resolve_token(&state, &headers).await?;

    // Generate 8-char alphanumeric code: XXXX-YYYY
    let part = |n: u32| -> String {
        let chars: Vec<char> = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".chars().collect();
        (0..4).map(|i| chars[((n >> (i * 5)) & 0x1F) as usize % chars.len()]).collect()
    };
    let r: u32 = rand::random();
    let code = format!("{}-{}", part(r), part(r >> 16));

    sqlx::query("INSERT INTO invites (code, created_by) VALUES (?,?)")
        .bind(&code)
        .bind(user_id)
        .execute(&state.pool)
        .await?;

    Ok((StatusCode::CREATED, Json(json!({ "code": code }))).into_response())
}

// ── helpers ───────────────────────────────────────────────────────────────────

pub async fn resolve_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<i64, AppError> {
    let raw = extract_bearer(headers)?;
    let hash = hash_token(raw);
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT user_id FROM tokens WHERE token_hash=?
         AND (expires_at IS NULL OR expires_at > datetime('now'))",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await?;
    row.map(|(id,)| id).ok_or(AppError::Unauthorized)
}

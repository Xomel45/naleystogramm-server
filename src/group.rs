use std::sync::Arc;
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use axum::extract::ws::{Message as WsMsg, WebSocket};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc::unbounded_channel;

// ECIES: X25519 + HKDF-SHA256 + AES-256-GCM
use x25519_dalek::{EphemeralSecret, PublicKey as X25519Pub};
use hkdf::Hkdf;
use sha2::Sha256;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use aes_gcm::aead::Aead;

use crate::{
    auth::{generate_token, hash_token},
    error::AppError,
    state::AppState,
};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/info",           get(group_info))
        .route("/group/join",     post(group_join))
        .route("/group/leave",    delete(group_leave))
        .route("/group/members",  get(group_members))
        .route("/group/history",  get(group_history))
        .route("/group/ws",       get(group_ws))
        .with_state(state)
}

// ── ECIES helpers ─────────────────────────────────────────────────────────────

/// Зашифровать plaintext для получателя с pubkey через X25519+HKDF+AES-256-GCM.
/// Формат вывода: ephemeral_pub(32) || nonce(12) || ciphertext+tag
fn ecies_encrypt(recipient_pub_bytes: &[u8; 32], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let recipient_pub = X25519Pub::from(*recipient_pub_bytes);

    let mut rng = rand::thread_rng();
    let ephemeral_secret = EphemeralSecret::random_from_rng(&mut rng);
    let ephemeral_pub = X25519Pub::from(&ephemeral_secret);

    let shared = ephemeral_secret.diffie_hellman(&recipient_pub);

    // HKDF-SHA256(ikm=shared, salt=[], info="naleys-group-key-v1")
    let hk = Hkdf::<Sha256>::new(None, shared.as_bytes());
    let mut key_bytes = [0u8; 32];
    hk.expand(b"naleys-group-key-v1", &mut key_bytes)
        .map_err(|e| anyhow::anyhow!("hkdf expand: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("aes key: {e}"))?;

    let mut nonce_bytes = [0u8; 12];
    rng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("aes encrypt: {e}"))?;

    let mut out = Vec::with_capacity(32 + 12 + ciphertext.len());
    out.extend_from_slice(ephemeral_pub.as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

// ── DB helpers ────────────────────────────────────────────────────────────────

async fn resolve_member(state: &AppState, headers: &HeaderMap) -> Result<(String, String), AppError> {
    let raw = crate::auth::extract_bearer(headers)?;
    let hash = hash_token(raw);
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT username, role FROM group_members WHERE token_hash=?")
            .bind(&hash)
            .fetch_optional(&state.pool)
            .await?;
    row.ok_or(AppError::Unauthorized)
}

async fn get_or_create_group_key(pool: &sqlx::SqlitePool) -> anyhow::Result<Vec<u8>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM group_config WHERE key='group_key'")
            .fetch_optional(pool)
            .await?;
    if let Some((b64,)) = row {
        return Ok(B64.decode(b64)?);
    }
    let mut key = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    sqlx::query("INSERT INTO group_config (key, value) VALUES ('group_key', ?)")
        .bind(B64.encode(&key))
        .execute(pool)
        .await?;
    Ok(key)
}

async fn get_member_count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM group_members")
        .fetch_one(pool)
        .await
        .unwrap_or((0,))
        .0
}

// ── GET /info ─────────────────────────────────────────────────────────────────

async fn group_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let total = get_member_count(&state.pool).await;
    let online = state.group_conns.read().await.len() as i64;
    let mode = if state.config.group.broadcast_only { "channel" } else { "group" };

    Json(json!({
        "name":           state.config.group.name,
        "description":    state.config.group.description,
        "mode":           mode,
        "invite_only":    state.config.group.invite_only,
        "broadcast_only": state.config.group.broadcast_only,
        "members_total":  total,
        "members_online": online,
    }))
    .into_response()
}

// ── POST /group/join ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct JoinReq {
    username: String,
    pubkey:   String,
}

async fn group_join(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JoinReq>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.group.max_members > 0 {
        let cnt = get_member_count(&state.pool).await;
        if cnt >= state.config.group.max_members as i64 {
            return Ok((StatusCode::FORBIDDEN, Json(json!({ "error": "group_full" }))).into_response());
        }
    }

    let key_bytes = B64
        .decode(&body.pubkey)
        .map_err(|_| AppError::BadRequest("invalid pubkey".into()))?;
    if key_bytes.len() != 32 {
        return Err(AppError::BadRequest("pubkey must be 32 bytes".into()));
    }
    let pub_array: [u8; 32] = key_bytes.try_into().unwrap();

    let group_key = get_or_create_group_key(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let group_key_enc = ecies_encrypt(&pub_array, &group_key)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let token = generate_token();
    let token_hash = hash_token(&token);

    // Первый вошедший становится owner
    let is_first = get_member_count(&state.pool).await == 0;
    let role = if is_first { "owner" } else { "member" };

    sqlx::query(
        "INSERT INTO group_members (username, pubkey, token_hash, role)
         VALUES (?,?,?,?)
         ON CONFLICT(username) DO UPDATE SET pubkey=excluded.pubkey, token_hash=excluded.token_hash",
    )
    .bind(&body.username)
    .bind(&body.pubkey)
    .bind(&token_hash)
    .bind(role)
    .execute(&state.pool)
    .await?;

    broadcast(&state, json!({ "type": "join", "username": body.username, "role": role }).to_string()).await;

    Ok(Json(json!({
        "token":         token,
        "group_key_enc": B64.encode(&group_key_enc),
        "role":          role,
    }))
    .into_response())
}

// ── DELETE /group/leave ───────────────────────────────────────────────────────

async fn group_leave(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let (username, _role) = resolve_member(&state, &headers).await?;
    sqlx::query("DELETE FROM group_members WHERE username=?")
        .bind(&username)
        .execute(&state.pool)
        .await?;
    broadcast(&state, json!({ "type": "leave", "username": username }).to_string()).await;
    Ok((StatusCode::NO_CONTENT, ()).into_response())
}

// ── GET /group/members ────────────────────────────────────────────────────────

async fn group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    resolve_member(&state, &headers).await?;
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT username, role, joined_at FROM group_members ORDER BY username")
            .fetch_all(&state.pool)
            .await?;
    let online = state.group_conns.read().await;
    let members: Vec<Value> = rows
        .iter()
        .map(|(u, role, joined)| {
            json!({
                "username":  u,
                "role":      role,
                "online":    online.contains_key(u.as_str()),
                "joined_at": joined,
            })
        })
        .collect();
    Ok(Json(json!({ "members": members })).into_response())
}

// ── GET /group/history ────────────────────────────────────────────────────────

async fn group_history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    resolve_member(&state, &headers).await?;
    let limit = state.config.group.history_limit as i64;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT sender, data, ts FROM group_messages ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    let msgs: Vec<Value> = rows
        .into_iter()
        .rev()
        .map(|(sender, data, ts)| json!({ "sender": sender, "data": data, "ts": ts }))
        .collect();
    Ok(Json(json!({ "messages": msgs })).into_response())
}

// ── GET /group/ws (WebSocket) ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct GroupWsQuery {
    token: Option<String>,
}

async fn group_ws(
    ws: WebSocketUpgrade,
    Query(q): Query<GroupWsQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(token) = q.token else {
        return (StatusCode::BAD_REQUEST, "missing token").into_response();
    };
    let hash = hash_token(&token);
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT username, role FROM group_members WHERE token_hash=?")
            .bind(&hash)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let Some((username, role)) = row else {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    };

    ws.on_upgrade(move |socket| group_ws_handler(socket, username, role, state))
        .into_response()
}

async fn group_ws_handler(socket: WebSocket, username: String, role: String, state: Arc<AppState>) {
    let (tx, mut rx) = unbounded_channel::<String>();

    state.group_conns.write().await.insert(username.clone(), tx);

    // Отправить историю при подключении
    let history_limit = state.config.group.history_limit as i64;
    let history: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT sender, data, ts FROM group_messages ORDER BY id DESC LIMIT ?",
    )
    .bind(history_limit)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let history_msgs: Vec<Value> = history
        .into_iter()
        .rev()
        .map(|(s, d, t)| json!({ "sender": s, "data": d, "ts": t }))
        .collect();

    let (mut sink, mut stream) = socket.split();

    if state.config.group.history {
        let hist_frame = json!({ "type": "history", "messages": history_msgs }).to_string();
        sink.send(WsMsg::Text(hist_frame.into())).await.ok();
    }

    let broadcast_only = state.config.group.broadcast_only;

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(WsMsg::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            if v["type"] == "msg" {
                                // В режиме канала только owner/admin могут отправлять
                                if broadcast_only && role != "owner" && role != "admin" {
                                    let err = json!({ "type": "error", "code": "forbidden", "detail": "only admins can post in a channel" }).to_string();
                                    sink.send(WsMsg::Text(err.into())).await.ok();
                                    continue;
                                }
                                let ts = chrono::Utc::now().timestamp();
                                let data = v["data"].as_str().unwrap_or("").to_string();
                                let _ = sqlx::query(
                                    "INSERT INTO group_messages (sender, data, ts) VALUES (?,?,?)",
                                )
                                .bind(&username)
                                .bind(&data)
                                .bind(ts)
                                .execute(&state.pool)
                                .await;
                                // Обрезать историю
                                let _ = sqlx::query(
                                    "DELETE FROM group_messages WHERE id NOT IN
                                     (SELECT id FROM group_messages ORDER BY id DESC LIMIT ?)",
                                )
                                .bind(history_limit)
                                .execute(&state.pool)
                                .await;
                                let out = json!({
                                    "type":   "msg",
                                    "sender": username,
                                    "data":   data,
                                    "ts":     ts,
                                })
                                .to_string();
                                broadcast(&state, out).await;
                            }
                        }
                    }
                    Some(Ok(WsMsg::Ping(d))) => { sink.send(WsMsg::Pong(d)).await.ok(); }
                    _ => break,
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(s) => {
                        if sink.send(WsMsg::Text(s.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    state.group_conns.write().await.remove(&username);
    broadcast(&state, json!({ "type": "leave", "username": username }).to_string()).await;
}

async fn broadcast(state: &AppState, msg: String) {
    let conns = state.group_conns.read().await;
    for tx in conns.values() {
        tx.send(msg.clone()).ok();
    }
}

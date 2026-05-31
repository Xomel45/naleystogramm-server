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

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn resolve_member(state: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    let raw = crate::auth::extract_bearer(headers)?;
    let hash = hash_token(raw);
    let row: Option<(String,)> =
        sqlx::query_as("SELECT username FROM group_members WHERE token_hash=?")
            .bind(&hash)
            .fetch_optional(&state.pool)
            .await?;
    row.map(|(u,)| u).ok_or(AppError::Unauthorized)
}

async fn get_or_create_group_key(pool: &sqlx::SqlitePool) -> anyhow::Result<Vec<u8>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM group_config WHERE key='group_key'")
            .fetch_optional(pool)
            .await?;
    if let Some((b64,)) = row {
        return Ok(B64.decode(b64)?);
    }
    // Generate fresh AES-256 key
    let mut key = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    sqlx::query("INSERT INTO group_config (key, value) VALUES ('group_key', ?)")
        .bind(B64.encode(&key))
        .execute(pool)
        .await?;
    Ok(key)
}

// ── GET /info (group mode) ────────────────────────────────────────────────────

async fn group_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (total,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM group_members")
            .fetch_one(&state.pool)
            .await
            .unwrap_or((0,));
    let online = state.group_conns.read().await.len() as i64;

    Json(json!({
        "name":        state.config.group.name,
        "description": state.config.group.description,
        "mode":        "group",
        "invite_only": state.config.group.invite_only,
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
    // Max members
    if state.config.group.max_members > 0 {
        let (cnt,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM group_members")
                .fetch_one(&state.pool)
                .await?;
        if cnt >= state.config.group.max_members as i64 {
            return Ok((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "group_full" })),
            )
                .into_response());
        }
    }

    // Pubkey validation
    let key_bytes = B64
        .decode(&body.pubkey)
        .map_err(|_| AppError::BadRequest("invalid pubkey".into()))?;
    if key_bytes.len() != 32 {
        return Err(AppError::BadRequest("pubkey must be 32 bytes".into()));
    }

    // Get or create group key
    let group_key = get_or_create_group_key(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // For now: XOR-encrypt group key with first 32 bytes of pubkey (placeholder).
    // A real implementation would use ECIES with X25519 + HKDF + AES-GCM.
    let group_key_enc: Vec<u8> = group_key
        .iter()
        .zip(key_bytes.iter().cycle())
        .map(|(a, b)| a ^ b)
        .collect();

    let token = generate_token();
    let token_hash = hash_token(&token);

    sqlx::query(
        "INSERT INTO group_members (username, pubkey, token_hash)
         VALUES (?,?,?)
         ON CONFLICT(username) DO UPDATE SET pubkey=excluded.pubkey, token_hash=excluded.token_hash",
    )
    .bind(&body.username)
    .bind(&body.pubkey)
    .bind(&token_hash)
    .execute(&state.pool)
    .await?;

    // Broadcast join event
    broadcast(&state, json!({ "type": "join", "username": body.username }).to_string()).await;

    Ok(Json(json!({
        "token":         token,
        "group_key_enc": B64.encode(&group_key_enc),
    }))
    .into_response())
}

// ── DELETE /group/leave ───────────────────────────────────────────────────────

async fn group_leave(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let username = resolve_member(&state, &headers).await?;
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
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT username, joined_at FROM group_members ORDER BY username")
            .fetch_all(&state.pool)
            .await?;
    let online = state.group_conns.read().await;
    let members: Vec<Value> = rows
        .iter()
        .map(|(u, joined)| {
            json!({
                "username":  u,
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
    let username: Option<(String,)> =
        sqlx::query_as("SELECT username FROM group_members WHERE token_hash=?")
            .bind(&hash)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let Some((username,)) = username else {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    };

    ws.on_upgrade(move |socket| group_ws_handler(socket, username, state))
        .into_response()
}

async fn group_ws_handler(socket: WebSocket, username: String, state: Arc<AppState>) {
    let (tx, mut rx) = unbounded_channel::<String>();

    // Register connection
    state
        .group_conns
        .write()
        .await
        .insert(username.clone(), tx);

    // Send history on connect
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
        let hist_frame =
            json!({ "type": "history", "messages": history_msgs }).to_string();
        sink.send(WsMsg::Text(hist_frame.into())).await.ok();
    }

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(WsMsg::Text(text))) => {
                        // Parse incoming message
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            if v["type"] == "msg" {
                                let ts = chrono::Utc::now().timestamp();
                                let data = v["data"].as_str().unwrap_or("").to_string();
                                // Store in DB
                                let _ = sqlx::query(
                                    "INSERT INTO group_messages (sender, data, ts) VALUES (?,?,?)",
                                )
                                .bind(&username)
                                .bind(&data)
                                .bind(ts)
                                .execute(&state.pool)
                                .await;
                                // Trim history
                                let _ = sqlx::query(
                                    "DELETE FROM group_messages WHERE id NOT IN
                                     (SELECT id FROM group_messages ORDER BY id DESC LIMIT ?)",
                                )
                                .bind(history_limit)
                                .execute(&state.pool)
                                .await;
                                // Broadcast to all
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

    // Unregister
    state.group_conns.write().await.remove(&username);
    broadcast(&state, json!({ "type": "leave", "username": username }).to_string()).await;
}

async fn broadcast(state: &AppState, msg: String) {
    let conns = state.group_conns.read().await;
    for tx in conns.values() {
        tx.send(msg.clone()).ok();
    }
}

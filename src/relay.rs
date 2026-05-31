use std::sync::Arc;
use std::time::{Duration, Instant};
use axum::{
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum::extract::ws::{Message as WsMsg, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

use crate::{
    auth::hash_token,
    discovery::resolve_token,
    error::AppError,
    state::{AppState, InitializedRelay, PendingRelay},
};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/relay/create",    post(relay_create))
        .route("/relay/join/{id}", post(relay_join))
        .route("/relay/{id}",      get(relay_ws))
        .with_state(state)
}

// ── POST /relay/create ────────────────────────────────────────────────────────

async fn relay_create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.relay.enabled {
        return Err(AppError::Forbidden("relay_disabled"));
    }
    let creator_id = resolve_token(&state, &headers).await?;

    // Max sessions check
    if state.config.relay.max_sessions > 0 {
        let cnt = {
            let i = state.relay_init.lock().await;
            let p = state.relay_pending.lock().await;
            i.len() + p.len()
        };
        if cnt >= state.config.relay.max_sessions as usize {
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "max_sessions_reached" })),
            )
                .into_response());
        }
    }

    let session_id = Uuid::new_v4().to_string();

    // token_a: returned to creator
    let token_a_raw = crate::auth::generate_token();
    let token_a_hash = hash_token(&token_a_raw);

    // token_b: stored plain for one-time retrieval by joiner
    let token_b_raw = crate::auth::generate_token();
    let token_b_hash = hash_token(&token_b_raw);

    // Persist in DB for audit
    sqlx::query("INSERT INTO relay_sessions (id, creator_id) VALUES (?,?)")
        .bind(&session_id)
        .bind(creator_id)
        .execute(&state.pool)
        .await?;

    state.relay_init.lock().await.insert(
        session_id.clone(),
        InitializedRelay {
            token_a_hash,
            token_b: token_b_raw,
            token_b_hash,
            creator_id,
            created_at: Instant::now(),
        },
    );

    Ok(Json(json!({
        "session_id": session_id,
        "token_a":    token_a_raw,
    }))
    .into_response())
}

// ── POST /relay/join/:id ──────────────────────────────────────────────────────

async fn relay_join(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.relay.enabled {
        return Err(AppError::Forbidden("relay_disabled"));
    }
    let joiner_id = resolve_token(&state, &headers).await?;

    let token_b = {
        let guard = state.relay_init.lock().await;
        guard
            .get(&session_id)
            .map(|s| s.token_b.clone())
            .ok_or(AppError::NotFound("session_not_found"))?
    };

    // Record joiner in DB
    sqlx::query("UPDATE relay_sessions SET joiner_id=? WHERE id=?")
        .bind(joiner_id)
        .bind(&session_id)
        .execute(&state.pool)
        .await?;

    Ok(Json(json!({ "token_b": token_b })).into_response())
}

// ── GET /relay/:id (WebSocket) ────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct RelayWsQuery {
    token: Option<String>,
}

async fn relay_ws(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(q): Query<RelayWsQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(token) = q.token else {
        return (StatusCode::BAD_REQUEST, "missing token").into_response();
    };
    let token_hash = hash_token(&token);

    // Try initialized map first (no WS yet — this must be the creator)
    let maybe_init = {
        let guard = state.relay_init.lock().await;
        guard.get(&session_id).cloned()
    };

    if let Some(init) = maybe_init {
        if token_hash != init.token_a_hash {
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }

        // Transition to pending
        let (a_to_b_tx, a_to_b_rx_raw) = unbounded_channel::<WsMsg>();
        let (b_to_a_tx, b_to_a_rx_raw) = unbounded_channel::<WsMsg>();

        let pending = Arc::new(PendingRelay {
            token_b_hash:  init.token_b_hash.clone(),
            a_to_b_tx,
            a_to_b_rx:    tokio::sync::Mutex::new(Some(a_to_b_rx_raw)),
            b_to_a_tx,
            b_to_a_rx:    tokio::sync::Mutex::new(Some(b_to_a_rx_raw)),
            joiner_ready: tokio::sync::Notify::new(),
            created_at:   Instant::now(),
        });

        {
            let mut guard = state.relay_init.lock().await;
            guard.remove(&session_id);
        }
        {
            let mut guard = state.relay_pending.lock().await;
            guard.insert(session_id.clone(), Arc::clone(&pending));
        }

        let timeout_secs = state.config.relay.session_timeout_seconds;
        let sid = session_id.clone();
        let st  = Arc::clone(&state);
        return ws
            .on_upgrade(move |socket| creator_ws(socket, pending, sid, st, timeout_secs))
            .into_response();
    }

    // Try pending (joiner connecting)
    let maybe_pending = {
        let mut guard = state.relay_pending.lock().await;
        if guard
            .get(&session_id)
            .map(|p| p.token_b_hash == token_hash)
            .unwrap_or(false)
        {
            guard.remove(&session_id)
        } else {
            None
        }
    };

    if let Some(pending) = maybe_pending {
        return ws
            .on_upgrade(move |socket| joiner_ws(socket, pending))
            .into_response();
    }

    (StatusCode::NOT_FOUND, "session not found").into_response()
}

// ── Creator WebSocket handler ─────────────────────────────────────────────────

async fn creator_ws(
    socket: WebSocket,
    pending: Arc<PendingRelay>,
    session_id: String,
    state: Arc<AppState>,
    timeout_secs: u64,
) {
    let mut b_to_a_rx = match pending.b_to_a_rx.lock().await.take() {
        Some(rx) => rx,
        None => return,
    };

    let (mut sink, mut stream) = socket.split();
    let a_to_b_tx = pending.a_to_b_tx.clone();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut pre_buffer: Vec<WsMsg> = Vec::new();

    // Phase 1: wait for joiner, buffering A's messages
    loop {
        tokio::select! {
            _ = pending.joiner_ready.notified() => break,
            msg = stream.next() => {
                match msg {
                    Some(Ok(m)) if data_msg(&m) => pre_buffer.push(m),
                    Some(Ok(WsMsg::Ping(d))) => { sink.send(WsMsg::Pong(d)).await.ok(); }
                    None | Some(Err(_)) | Some(Ok(WsMsg::Close(_))) => {
                        state.relay_pending.lock().await.remove(&session_id);
                        return;
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                state.relay_pending.lock().await.remove(&session_id);
                return;
            }
        }
    }

    // Flush buffered messages
    for m in pre_buffer {
        a_to_b_tx.send(m).ok();
    }

    // Phase 2: bidirectional relay
    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(m)) if data_msg(&m) => { a_to_b_tx.send(m).ok(); }
                    Some(Ok(WsMsg::Ping(d))) => { sink.send(WsMsg::Pong(d)).await.ok(); }
                    _ => break,
                }
            }
            msg = b_to_a_rx.recv() => {
                match msg {
                    Some(m) => { if sink.send(m).await.is_err() { break; } }
                    None => break,
                }
            }
        }
    }
}

// ── Joiner WebSocket handler ──────────────────────────────────────────────────

async fn joiner_ws(socket: WebSocket, pending: Arc<PendingRelay>) {
    let mut a_to_b_rx = match pending.a_to_b_rx.lock().await.take() {
        Some(rx) => rx,
        None => return,
    };

    pending.joiner_ready.notify_one();

    let (mut sink, mut stream) = socket.split();
    let b_to_a_tx = pending.b_to_a_tx.clone();

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(m)) if data_msg(&m) => { b_to_a_tx.send(m).ok(); }
                    Some(Ok(WsMsg::Ping(d))) => { sink.send(WsMsg::Pong(d)).await.ok(); }
                    _ => break,
                }
            }
            msg = a_to_b_rx.recv() => {
                match msg {
                    Some(m) => { if sink.send(m).await.is_err() { break; } }
                    None => break,
                }
            }
        }
    }
}

fn data_msg(m: &WsMsg) -> bool {
    matches!(m, WsMsg::Text(_) | WsMsg::Binary(_))
}

// ── Background cleanup ────────────────────────────────────────────────────────

pub async fn cleanup_loop(state: Arc<AppState>) {
    let interval = Duration::from_secs(60);
    loop {
        tokio::time::sleep(interval).await;
        let timeout = Duration::from_secs(state.config.relay.session_timeout_seconds);
        let grace  = timeout + Duration::from_secs(30);
        let now = Instant::now();

        state.relay_init.lock().await.retain(|_, v| {
            now.duration_since(v.created_at) < grace
        });
        // Safety net: remove pending sessions that somehow leaked past creator timeout
        state.relay_pending.lock().await.retain(|_, v| {
            now.duration_since(v.created_at) < grace
        });
    }
}

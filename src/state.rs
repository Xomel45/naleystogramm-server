use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tokio::sync::mpsc::UnboundedSender;
use axum::extract::ws::Message as WsMsg;
use crate::config::Config;
use crate::rate_limit::RateLimiter;

/// Relay session created via POST /relay/create — no WS yet.
#[derive(Clone)]
#[allow(dead_code)]
pub struct InitializedRelay {
    pub token_a_hash:   String,
    pub token_b:        String,   // plain — returned once on /relay/join
    pub token_b_hash:   String,
    pub creator_id:     i64,
    pub created_at:     Instant,
}

/// Creator WS connected; waiting for joiner.
#[allow(dead_code)]
pub struct PendingRelay {
    pub token_b_hash: String,
    /// Creator (A) writes here → joiner (B) reads
    pub a_to_b_tx:    UnboundedSender<WsMsg>,
    pub a_to_b_rx:    Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<WsMsg>>>,
    /// Joiner (B) writes here → creator (A) reads
    pub b_to_a_tx:    UnboundedSender<WsMsg>,
    pub b_to_a_rx:    Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<WsMsg>>>,
    pub joiner_ready: tokio::sync::Notify,
    pub created_at:   Instant,
}

pub struct AppState {
    pub pool:          sqlx::SqlitePool,
    pub config:        Config,
    pub mode:          String,
    /// Effective min_client_version (updated by background task when "latest")
    pub min_version:   RwLock<String>,
    pub rate_limiter:  RateLimiter,
    /// session_id → InitializedRelay (before any WS connects)
    pub relay_init:    Mutex<HashMap<String, InitializedRelay>>,
    /// session_id → Arc<PendingRelay> (creator connected, joiner not yet)
    pub relay_pending: Mutex<HashMap<String, Arc<PendingRelay>>>,
    /// Group broadcast: username → per-connection sender
    pub group_conns:   RwLock<HashMap<String, UnboundedSender<String>>>,
}

impl AppState {
    pub fn new(pool: sqlx::SqlitePool, config: Config, mode: String) -> Arc<Self> {
        let min_version = config.compatibility.min_client_version.clone();
        let initial_version = if min_version == "latest" {
            "0.0.0".to_string()
        } else {
            min_version
        };
        Arc::new(Self {
            pool,
            config,
            mode,
            min_version:   RwLock::new(initial_version),
            rate_limiter:  RateLimiter::new(),
            relay_init:    Mutex::new(HashMap::new()),
            relay_pending: Mutex::new(HashMap::new()),
            group_conns:   RwLock::new(HashMap::new()),
        })
    }
}

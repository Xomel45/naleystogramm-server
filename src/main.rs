mod auth;
mod config;
mod db;
mod discovery;
mod error;
mod group;
mod rate_limit;
mod relay;
mod state;
mod version;

use std::net::SocketAddr;
use std::sync::Arc;
use anyhow::Context;
use axum::Router;
use clap::Parser;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

#[derive(Parser)]
#[command(name = "naleys-server", about = "Naleystogramm Discovery & Relay Server")]
struct Args {
    /// Path to config.json
    #[arg(long, default_value = "config.json")]
    config: String,

    /// Server mode: discovery (default) or group
    #[arg(long, default_value = "discovery")]
    mode: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "naleys_server=info,tower_http=warn".into()),
        )
        .init();

    let args = Args::parse();

    let config_str = std::fs::read_to_string(&args.config)
        .with_context(|| format!("cannot read config file '{}'", args.config))?;
    let config: config::Config = serde_json::from_str(&config_str)
        .context("invalid config.json")?;

    tracing::info!(
        "starting naleys-server v{} | mode={} | port={}",
        env!("CARGO_PKG_VERSION"),
        args.mode,
        config.server.port
    );

    let pool = db::create_pool(&config.storage)
        .await
        .context("failed to open database")?;
    db::migrate(&pool, &args.mode)
        .await
        .context("migration failed")?;

    let state = state::AppState::new(pool, config.clone(), args.mode.clone());

    // Background: refresh min_client_version from GitHub
    if config.compatibility.min_client_version == "latest" {
        tokio::spawn(version::refresh_loop(Arc::clone(&state)));
    }

    // Background: cleanup stale relay sessions
    tokio::spawn(relay::cleanup_loop(Arc::clone(&state)));

    let app = build_router(Arc::clone(&state), &args.mode)
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot bind to {addr}"))?;

    tracing::info!("listening on {}", addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn build_router(state: Arc<state::AppState>, mode: &str) -> Router {
    match mode {
        "group" | "channel" => Router::new()
            .merge(group::router(Arc::clone(&state)))
            .merge(relay::router(Arc::clone(&state))),
        _ => Router::new()
            .merge(discovery::router(Arc::clone(&state)))
            .merge(relay::router(Arc::clone(&state))),
    }
}

use std::sync::Arc;
use tokio::time::{Duration, sleep};
use crate::state::AppState;

pub async fn refresh_loop(state: Arc<AppState>) {
    loop {
        match fetch_latest().await {
            Ok(ver) => {
                tracing::info!("min_client_version updated to {}", ver);
                *state.min_version.write().await = ver;
            }
            Err(e) => tracing::warn!("failed to fetch latest version: {}", e),
        }
        sleep(Duration::from_secs(6 * 3600)).await;
    }
}

async fn fetch_latest() -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("naleys-server/0.1.0")
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/Xomel45/naleystogramm/releases/latest")
        .send()
        .await?
        .json()
        .await?;
    let tag = resp["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no tag_name in response"))?;
    Ok(tag.trim_start_matches('v').to_string())
}

pub fn satisfies(client_version: &str, min_version: &str) -> bool {
    let parse = |s: &str| semver::Version::parse(s).ok();
    match (parse(client_version), parse(min_version)) {
        (Some(c), Some(m)) => c >= m,
        _ => true,
    }
}

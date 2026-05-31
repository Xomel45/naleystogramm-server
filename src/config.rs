#![allow(dead_code)]
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server:        ServerConfig,
    pub registration:  RegistrationConfig,
    pub tokens:        TokensConfig,
    pub storage:       StorageConfig,
    pub presence:      PresenceConfig,
    pub compatibility: CompatibilityConfig,
    #[serde(default)]
    pub relay:         RelayConfig,
    #[serde(default)]
    pub group:         GroupConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port:        u16,
    #[serde(default = "default_server_name")]
    pub name:        String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub public:      bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RegistrationConfig {
    #[serde(default = "default_true")]
    pub open:                 bool,
    #[serde(default)]
    pub require_email:        bool,
    #[serde(default)]
    pub require_invite:       bool,
    #[serde(default)]
    pub max_users:            u32,
    #[serde(default = "default_username_min")]
    pub username_min_length:  usize,
    #[serde(default = "default_username_max")]
    pub username_max_length:  usize,
    #[serde(default = "default_username_regex")]
    pub username_regex:       String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TokensConfig {
    #[serde(default)]
    pub ttl_days:     u32,
    #[serde(default = "default_max_per_user")]
    pub max_per_user: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    #[serde(default = "default_driver")]
    pub driver: String,
    #[serde(default = "default_db_path")]
    pub path:   String,
    #[serde(default)]
    pub url:    String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PresenceConfig {
    #[serde(default = "default_offline_after")]
    pub offline_after_minutes:        u32,
    #[serde(default = "default_ip_update_interval")]
    pub ip_update_interval_seconds:   u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CompatibilityConfig {
    #[serde(default = "default_min_version")]
    pub min_client_version: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RelayConfig {
    #[serde(default = "default_true")]
    pub enabled:                 bool,
    #[serde(default)]
    pub max_sessions:            u32,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_seconds: u64,
    #[serde(default)]
    pub max_session_bytes:       u64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            enabled:                 true,
            max_sessions:            0,
            session_timeout_seconds: 30,
            max_session_bytes:       0,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct GroupConfig {
    #[serde(default)]
    pub name:             String,
    #[serde(default)]
    pub description:      String,
    #[serde(default)]
    pub max_members:      u32,
    #[serde(default)]
    pub invite_only:      bool,
    #[serde(default = "default_true")]
    pub history:          bool,
    #[serde(default = "default_history_limit")]
    pub history_limit:    u32,
    #[serde(default = "default_true")]
    pub allow_file_relay: bool,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            name:             String::new(),
            description:      String::new(),
            max_members:      0,
            invite_only:      false,
            history:          true,
            history_limit:    1000,
            allow_file_relay: true,
        }
    }
}

fn default_true() -> bool            { true }
fn default_port() -> u16             { 47822 }
fn default_server_name() -> String   { "Naleystogramm Server".into() }
fn default_username_min() -> usize   { 3 }
fn default_username_max() -> usize   { 32 }
fn default_username_regex() -> String { r"^[a-zA-Z0-9_.\-]+$".into() }
fn default_max_per_user() -> u32     { 3 }
fn default_driver() -> String        { "sqlite".into() }
fn default_db_path() -> String       { "./naleys_server.db".into() }
fn default_offline_after() -> u32    { 30 }
fn default_ip_update_interval() -> u32 { 60 }
fn default_min_version() -> String   { "latest".into() }
fn default_session_timeout() -> u64  { 30 }
fn default_history_limit() -> u32    { 1000 }

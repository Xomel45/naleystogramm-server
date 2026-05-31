use sqlx::sqlite::SqliteConnectOptions;
use std::str::FromStr;
use crate::config::StorageConfig;

pub async fn create_pool(cfg: &StorageConfig) -> anyhow::Result<sqlx::SqlitePool> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", cfg.path))?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    Ok(pool)
}

pub async fn migrate(pool: &sqlx::SqlitePool, mode: &str) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            username    TEXT    UNIQUE NOT NULL,
            username_lc TEXT    UNIQUE NOT NULL,
            pubkey      TEXT    NOT NULL,
            email       TEXT,
            created_at  TEXT    DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS presence (
            user_id   INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
            ip        TEXT    NOT NULL,
            port      INTEGER NOT NULL,
            last_seen TEXT    DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tokens (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            token_hash TEXT    UNIQUE NOT NULL,
            created_at TEXT    DEFAULT (datetime('now')),
            expires_at TEXT
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS invites (
            code       TEXT PRIMARY KEY,
            created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
            used_by    INTEGER REFERENCES users(id) ON DELETE SET NULL,
            created_at TEXT DEFAULT (datetime('now')),
            used_at    TEXT
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS relay_sessions (
            id          TEXT    PRIMARY KEY,
            creator_id  INTEGER REFERENCES users(id) ON DELETE SET NULL,
            joiner_id   INTEGER REFERENCES users(id) ON DELETE SET NULL,
            created_at  TEXT    DEFAULT (datetime('now')),
            bytes_total INTEGER DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;

    if mode == "group" {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS group_members (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                username   TEXT UNIQUE NOT NULL,
                pubkey     TEXT NOT NULL,
                token_hash TEXT UNIQUE NOT NULL,
                joined_at  TEXT DEFAULT (datetime('now'))
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS group_messages (
                id     INTEGER PRIMARY KEY AUTOINCREMENT,
                sender TEXT    NOT NULL,
                data   TEXT    NOT NULL,
                ts     INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS group_config (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await?;
    }

    Ok(())
}

use anyhow::Context;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{path::Path, str::FromStr};

const INIT_SQL: &str = include_str!("../../../migrations/0001_init.sql");
const INIT_MIGRATION_VERSION: &str = "0001_init";

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    ensure_sqlite_parent(database_url)?;

    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("parse database url {database_url}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_millis(5000));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("open database {database_url}"))?;

    sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
    sqlx::query("PRAGMA busy_timeout=5000")
        .execute(&pool)
        .await?;
    run_migrations(&pool).await?;
    cleanup_startup_state(&pool).await?;
    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    ensure_migrations_table(pool).await?;
    if !migration_applied(pool, INIT_MIGRATION_VERSION).await? {
        sqlx::query(INIT_SQL).execute(pool).await?;
        record_migration(pool, INIT_MIGRATION_VERSION).await?;
    }
    Ok(())
}

async fn ensure_migrations_table(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations ( \
         version TEXT PRIMARY KEY, \
         applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn migration_applied(pool: &SqlitePool, version: &str) -> anyhow::Result<bool> {
    let count = sqlx::query("SELECT COUNT(*) AS count FROM schema_migrations WHERE version = ?1")
        .bind(version)
        .fetch_one(pool)
        .await?
        .get::<i64, _>("count");
    Ok(count > 0)
}

async fn record_migration(pool: &SqlitePool, version: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO schema_migrations (version) VALUES (?1)")
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}

async fn cleanup_startup_state(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tunnels SET status = 'disconnected', disconnected_at = CURRENT_TIMESTAMP \
         WHERE status = 'connected'",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "UPDATE tunnels SET status = 'expired' \
         WHERE status = 'reserved' AND expires_at <= CURRENT_TIMESTAMP",
    )
    .execute(pool)
    .await?;

    Ok(())
}

fn ensure_sqlite_parent(database_url: &str) -> anyhow::Result<()> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .unwrap_or(database_url);

    if path == ":memory:" || path.starts_with("file:") {
        return Ok(());
    }

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create database dir {}", parent.display()))?;
        }
    }
    Ok(())
}

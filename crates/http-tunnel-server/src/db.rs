use anyhow::Context;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{path::Path, str::FromStr};

const INIT_SQL: &str = include_str!("../../../schema/initial.sql");
const INIT_SCHEMA_VERSION: &str = "initial";

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
    initialize_schema(&pool).await?;
    cleanup_startup_state(&pool).await?;
    Ok(pool)
}

async fn initialize_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    ensure_schema_versions_table(pool).await?;
    if !schema_version_applied(pool, INIT_SCHEMA_VERSION).await? {
        sqlx::query(INIT_SQL).execute(pool).await?;
        record_schema_version(pool, INIT_SCHEMA_VERSION).await?;
    }
    ensure_column(pool, "sessions", "client_country_code", "TEXT").await?;
    ensure_column(pool, "sessions", "client_country", "TEXT").await?;
    Ok(())
}

async fn ensure_schema_versions_table(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_versions ( \
         version TEXT PRIMARY KEY, \
         applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn schema_version_applied(pool: &SqlitePool, version: &str) -> anyhow::Result<bool> {
    let count = sqlx::query("SELECT COUNT(*) AS count FROM schema_versions WHERE version = ?1")
        .bind(version)
        .fetch_one(pool)
        .await?
        .get::<i64, _>("count");
    Ok(count > 0)
}

async fn record_schema_version(pool: &SqlitePool, version: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO schema_versions (version) VALUES (?1)")
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    column_type: &str,
) -> anyhow::Result<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;
    if rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column)
    {
        return Ok(());
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
    sqlx::query(&sql).execute(pool).await?;
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

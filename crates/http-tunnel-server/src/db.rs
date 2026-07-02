use anyhow::Context;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{path::Path, str::FromStr};

const INIT_SQL: &str = include_str!("../../../schema/initial.sql");
const INIT_SCHEMA_VERSION: &str = "initial";
const TUNNEL_CLAIMS_SCHEMA_VERSION: &str = "tunnel_claims_v1";

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
    ensure_column(pool, "sessions", "client_reported_ip", "TEXT").await?;
    ensure_column(
        pool,
        "sessions",
        "client_reported_ip_updated_at",
        "TIMESTAMP",
    )
    .await?;
    ensure_column(pool, "sessions", "client_country_source", "TEXT").await?;
    ensure_column(pool, "sessions", "client_country_code", "TEXT").await?;
    ensure_column(pool, "sessions", "client_country", "TEXT").await?;
    ensure_column(pool, "tunnels", "owner_client_id", "TEXT").await?;
    ensure_column(pool, "tunnels", "owner_client_secret_hash", "TEXT").await?;
    ensure_column(pool, "tunnels", "client_ttl_seconds", "INTEGER").await?;
    ensure_column(pool, "tunnels", "claim_expires_at", "TIMESTAMP").await?;
    ensure_tunnel_claim_schema(pool).await?;
    ensure_dashboard_presence_schema(pool).await?;
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

async fn ensure_tunnel_claim_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    let applied = schema_version_applied(pool, TUNNEL_CLAIMS_SCHEMA_VERSION).await?;
    if !applied && tunnels_has_legacy_subdomain_unique(pool).await? {
        rebuild_tunnels_without_subdomain_unique(pool).await?;
    }
    ensure_claim_index(pool).await?;
    if !applied {
        record_schema_version(pool, TUNNEL_CLAIMS_SCHEMA_VERSION).await?;
    }
    Ok(())
}

async fn tunnels_has_legacy_subdomain_unique(pool: &SqlitePool) -> anyhow::Result<bool> {
    let indexes = sqlx::query("PRAGMA index_list(tunnels)")
        .fetch_all(pool)
        .await?;
    for index in indexes {
        let unique = index.try_get::<i64, _>("unique").unwrap_or_default() != 0;
        let partial = index.try_get::<i64, _>("partial").unwrap_or_default() != 0;
        if !unique || partial {
            continue;
        }
        let name = index.get::<String, _>("name");
        let columns = sqlx::query(&format!("PRAGMA index_info({name})"))
            .fetch_all(pool)
            .await?;
        if columns.len() == 1 && columns[0].get::<String, _>("name").eq("subdomain") {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn rebuild_tunnels_without_subdomain_unique(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *conn)
        .await?;
    let result = async {
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        sqlx::query(
            "CREATE TABLE tunnels_new ( \
             id TEXT PRIMARY KEY, \
             subdomain TEXT NOT NULL, \
             token_hash TEXT NOT NULL, \
             status TEXT NOT NULL, \
             enabled BOOLEAN NOT NULL DEFAULT TRUE, \
             created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             connected_at TIMESTAMP, \
             disconnected_at TIMESTAMP, \
             expires_at TIMESTAMP NOT NULL, \
             client_ip TEXT, \
             client_user_agent TEXT, \
             owner_client_id TEXT, \
             owner_client_secret_hash TEXT, \
             client_ttl_seconds INTEGER, \
             claim_expires_at TIMESTAMP, \
             access_policy TEXT NOT NULL DEFAULT 'public', \
             access_token_hash TEXT, \
             access_username TEXT, \
             access_password_hash TEXT, \
             allowed_methods TEXT, \
             blocked_path_prefixes TEXT, \
             inspector_enabled BOOLEAN NOT NULL DEFAULT FALSE, \
             rate_limit_per_minute INTEGER \
             )",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT INTO tunnels_new ( \
             id, subdomain, token_hash, status, enabled, created_at, connected_at, disconnected_at, \
             expires_at, client_ip, client_user_agent, owner_client_id, owner_client_secret_hash, \
             client_ttl_seconds, claim_expires_at, access_policy, access_token_hash, access_username, access_password_hash, \
             allowed_methods, blocked_path_prefixes, inspector_enabled, rate_limit_per_minute \
             ) \
             SELECT id, subdomain, token_hash, status, enabled, created_at, connected_at, disconnected_at, \
             expires_at, client_ip, client_user_agent, owner_client_id, owner_client_secret_hash, \
             NULL, claim_expires_at, access_policy, access_token_hash, access_username, access_password_hash, \
             allowed_methods, blocked_path_prefixes, inspector_enabled, rate_limit_per_minute FROM tunnels",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query("DROP TABLE tunnels").execute(&mut *conn).await?;
        sqlx::query("ALTER TABLE tunnels_new RENAME TO tunnels")
            .execute(&mut *conn)
            .await?;
        sqlx::query("COMMIT").execute(&mut *conn).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;

    if let Err(error) = result {
        let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        let _ = sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *conn)
            .await;
        return Err(error.into());
    }
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn ensure_claim_index(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tunnels_subdomain_claimed \
         ON tunnels(subdomain) WHERE status != 'deleted'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn ensure_dashboard_presence_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS dashboard_presence ( \
         session_id TEXT PRIMARY KEY, \
         last_seen_at INTEGER NOT NULL \
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_dashboard_presence_last_seen \
         ON dashboard_presence(last_seen_at DESC)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn cleanup_startup_state(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tunnels SET status = 'deleted', expires_at = CURRENT_TIMESTAMP, disconnected_at = CURRENT_TIMESTAMP, \
         claim_expires_at = CURRENT_TIMESTAMP \
         WHERE status != 'deleted' AND client_ttl_seconds IS NOT NULL AND expires_at <= CURRENT_TIMESTAMP",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "UPDATE tunnels SET status = 'disconnected', disconnected_at = CURRENT_TIMESTAMP, \
         claim_expires_at = datetime('now', '+1 hour') \
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn migrates_legacy_subdomain_unique_constraint() {
        let database = unique_test_db("legacy-subdomain-unique");
        if let Some(parent) = database.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let database_url = format!("sqlite://{}", database.display());
        let options = SqliteConnectOptions::from_str(&database_url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE schema_versions (version TEXT PRIMARY KEY, applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO schema_versions (version) VALUES ('initial')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE tunnels ( \
             id TEXT PRIMARY KEY, \
             subdomain TEXT NOT NULL UNIQUE, \
             token_hash TEXT NOT NULL, \
             status TEXT NOT NULL, \
             enabled BOOLEAN NOT NULL DEFAULT TRUE, \
             created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             connected_at TIMESTAMP, \
             disconnected_at TIMESTAMP, \
             expires_at TIMESTAMP NOT NULL, \
             client_ip TEXT, \
             client_user_agent TEXT, \
             client_ttl_seconds INTEGER, \
             access_policy TEXT NOT NULL DEFAULT 'public', \
             access_token_hash TEXT, \
             access_username TEXT, \
             access_password_hash TEXT, \
             allowed_methods TEXT, \
             blocked_path_prefixes TEXT, \
             inspector_enabled BOOLEAN NOT NULL DEFAULT FALSE, \
             rate_limit_per_minute INTEGER \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE sessions ( \
             id TEXT PRIMARY KEY, \
             tunnel_id TEXT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE, \
             connected_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             disconnected_at TIMESTAMP, \
             disconnect_reason TEXT, \
             last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             client_version TEXT, \
             client_capabilities TEXT, \
             remote_addr TEXT \
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let pool = connect(&database_url).await.unwrap();
        sqlx::query(
            "INSERT INTO tunnels (id, subdomain, token_hash, status, expires_at) VALUES \
             ('deleted_1', 'demo', 'hash', 'deleted', datetime('now', '+1 hour')), \
             ('deleted_2', 'demo', 'hash', 'deleted', datetime('now', '+1 hour'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tunnels (id, subdomain, token_hash, status, expires_at) \
             VALUES ('active_1', 'demo', 'hash', 'reserved', datetime('now', '+1 hour'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        let duplicate = sqlx::query(
            "INSERT INTO tunnels (id, subdomain, token_hash, status, expires_at) \
             VALUES ('active_2', 'demo', 'hash', 'reserved', datetime('now', '+1 hour'))",
        )
        .execute(&pool)
        .await;
        assert!(duplicate.is_err());
    }

    fn unique_test_db(name: &str) -> std::path::PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("target")
            .join("db-tests")
            .join(format!("{name}-{now}.sqlite3"))
    }
}

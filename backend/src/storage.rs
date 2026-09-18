use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
const LATEST_MIGRATION_VERSION: i64 = 16;

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    if let Some(path) = database_url.strip_prefix("sqlite://")
        && let Some(parent) = std::path::Path::new(path.split('?').next().unwrap_or(path)).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("invalid SQLite URL: {database_url}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let options = if database_url.contains(":memory:") {
        options
    } else {
        options.journal_mode(SqliteJournalMode::Wal)
    };
    let max_connections = if database_url.contains(":memory:") {
        1
    } else {
        5
    };
    let database_path = database_path(database_url);
    let database_existed = database_path
        .as_deref()
        .is_some_and(|path| path.is_file() && path.metadata().is_ok_and(|value| value.len() > 0));
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
        .context("failed to connect to SQLite")?;
    if database_existed && migration_is_pending(&pool).await? {
        let source = database_path
            .as_deref()
            .context("SQLite path is unavailable")?;
        let parent = source.parent().unwrap_or_else(|| Path::new("."));
        let backup_dir = parent.join("backups");
        std::fs::create_dir_all(&backup_dir).with_context(|| {
            format!(
                "failed to create migration backup directory {}",
                backup_dir.display()
            )
        })?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let destination = backup_dir.join(format!(
            "pre-migration-v{LATEST_MIGRATION_VERSION}-{stamp}-{}.db",
            Uuid::new_v4()
        ));
        backup_database(&pool, &destination).await?;
    }
    migrate(&pool).await?;
    Ok(pool)
}

pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("SQLite migration failed")?;
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .context("foreign key check failed")?;
    if !violations.is_empty() {
        bail!("foreign key check found {} violation(s)", violations.len());
    }
    let integrity = sqlx::query("PRAGMA integrity_check")
        .fetch_all(pool)
        .await
        .context("integrity check failed")?;
    let messages = integrity
        .iter()
        .filter_map(|row| row.try_get::<String, _>(0).ok())
        .collect::<Vec<_>>();
    if messages.as_slice() != ["ok"] {
        bail!("SQLite integrity check failed: {}", messages.join("; "));
    }
    Ok(())
}

pub async fn backup_database(pool: &SqlitePool, destination: &Path) -> Result<()> {
    if destination.exists() {
        bail!(
            "backup destination already exists: {}",
            destination.display()
        );
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create backup directory {}", parent.display()))?;
    }
    sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
        .execute(pool)
        .await
        .context("WAL checkpoint before backup failed")?;
    sqlx::query("VACUUM INTO ?")
        .bind(destination.to_string_lossy().as_ref())
        .execute(pool)
        .await
        .with_context(|| format!("SQLite backup failed: {}", destination.display()))?;
    verify_database_file(destination).await
}

pub async fn verify_database_file(path: &Path) -> Result<()> {
    let url = format!(
        "sqlite://{}?mode=ro",
        path.to_string_lossy().replace('\\', "/")
    );
    let options = SqliteConnectOptions::from_str(&url)
        .with_context(|| format!("invalid backup SQLite path: {}", path.display()))?
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open backup {}", path.display()))?;
    let messages = sqlx::query("PRAGMA integrity_check")
        .fetch_all(&pool)
        .await
        .context("backup integrity check failed")?
        .iter()
        .filter_map(|row| row.try_get::<String, _>(0).ok())
        .collect::<Vec<_>>();
    pool.close().await;
    if messages.as_slice() != ["ok"] {
        bail!("backup integrity check failed: {}", messages.join("; "));
    }
    Ok(())
}

async fn migration_is_pending(pool: &SqlitePool) -> Result<bool> {
    let table_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .context("failed to inspect migration metadata")?;
    if table_exists == 0 {
        return Ok(true);
    }
    let current: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
    )
    .fetch_one(pool)
    .await
    .context("failed to inspect applied migrations")?;
    Ok(current < LATEST_MIGRATION_VERSION)
}

fn database_path(database_url: &str) -> Option<PathBuf> {
    let value = database_url.strip_prefix("sqlite://")?;
    if value.contains(":memory:") {
        return None;
    }
    Some(PathBuf::from(value.split('?').next().unwrap_or(value)))
}

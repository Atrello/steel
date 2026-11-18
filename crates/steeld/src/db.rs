use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};

pub type Db = Pool<Sqlite>;

/// Open the SQLite database at `url` (e.g. `sqlite://steel.db?mode=rwc`) and bring its schema
/// up to date. Versioned migrations live in `crates/steeld/migrations` and are embedded in the
/// binary; each one runs once and is recorded in `_sqlx_migrations`.
pub async fn connect(url: &str) -> Result<Db, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&pool)
        .await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| sqlx::Error::Protocol(format!("migration failed: {e}")))?;
    Ok(pool)
}

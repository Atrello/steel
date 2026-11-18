use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};

pub type Db = Pool<Sqlite>;

/// Open the SQLite database at `url` (e.g. `sqlite://steel.db?mode=rwc`) and bring its schema
/// up to date. Versioned migrations live in `crates/steeld/migrations` and are embedded in the
/// binary; each one runs once and is recorded in `_sqlx_migrations`. Version numbers are
/// reserved per branch (see the README), so gaps and late-merged lower versions are expected.
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
    // Migrations from other branches may already be applied to this database but missing from
    // this binary, for example when a branch with a different migration number has merged first.
    // Versions need not be contiguous, and a lower version added later still runs once.
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
        .run(&pool)
        .await
        .map_err(|e| sqlx::Error::Protocol(format!("migration failed: {e}")))?;
    Ok(pool)
}

use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};

pub type Db = Pool<Sqlite>;

const MIGRATIONS: &str = include_str!("../migrations.sql");

/// Open the SQLite database at `url` (e.g. `sqlite://steel.db?mode=rwc`) and apply the schema.
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
    for stmt in MIGRATIONS
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(stmt).execute(&pool).await?;
    }
    Ok(pool)
}

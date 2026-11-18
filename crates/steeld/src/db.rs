use std::borrow::Cow;
use std::sync::OnceLock;

use sqlx::any::{install_default_drivers, AnyPoolOptions};
use sqlx::AnyPool;

/// The database pool. SQLite is the default. Postgres is selected by a `postgres://` URL.
pub type Db = AnyPool;

static POSTGRES: OnceLock<bool> = OnceLock::new();

/// True when the server runs on Postgres. Set once by [`connect`].
pub fn is_postgres() -> bool {
    POSTGRES.get().copied().unwrap_or(false)
}

/// Adapt a query written with `?` placeholders to the connected database.
///
/// Every query in steeld is written with `?` placeholders, which SQLite accepts. Postgres wants
/// numbered `$1`, `$2` placeholders, so on Postgres this rewrites each `?` outside a string
/// literal in order. On SQLite the text is borrowed unchanged.
///
/// Use it at every call site: `sqlx::query(&sql("SELECT ... WHERE id = ?"))`.
pub fn sql(text: &str) -> Cow<'_, str> {
    if !is_postgres() || !text.contains('?') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0_u32;
    let mut in_literal = false;
    for c in text.chars() {
        match c {
            '\'' => {
                in_literal = !in_literal;
                out.push(c);
            }
            '?' if !in_literal => {
                index += 1;
                out.push('$');
                out.push_str(&index.to_string());
            }
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Open the database at `url` and bring its schema up to date.
///
/// `sqlite://...` opens a SQLite file (foreign keys and WAL are enabled on every connection).
/// `postgres://...` connects to Postgres. Versioned migrations are embedded in the binary; each
/// one runs once and is recorded in `_sqlx_migrations`. Version numbers are reserved per branch
/// (see the README), so gaps and late-merged lower versions are expected.
pub async fn connect(url: &str) -> Result<Db, sqlx::Error> {
    install_default_drivers();
    let postgres = url.starts_with("postgres://") || url.starts_with("postgresql://");
    let _ = POSTGRES.set(postgres);
    if postgres {
        return Err(sqlx::Error::Protocol(
            "Postgres support is not available in this build yet".into(),
        ));
    }
    let pool = AnyPoolOptions::new()
        .max_connections(8)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query(&crate::db::sql("PRAGMA foreign_keys = ON"))
                    .execute(&mut *conn)
                    .await?;
                sqlx::query(&crate::db::sql("PRAGMA journal_mode = WAL"))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
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

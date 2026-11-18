#![allow(clippy::result_large_err)]
mod api;
mod audit;
mod auth;
mod bots;
mod channels;
mod chat;
mod db;
mod dm;
mod emojis;
mod events;
mod gateway;
mod invite_page;
mod member_list;
mod members;
mod onboarding;
mod perms;
mod pins;
mod polls;
mod presence;
mod profile;
mod reactions;
mod roles;
mod search;
mod session;
mod settings;
mod state;
mod threads;
mod uploads;

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "steeld=info".into()),
        )
        .init();

    let database_url =
        std::env::var("STEEL_DATABASE_URL").unwrap_or_else(|_| "sqlite://steel.db?mode=rwc".into());
    let bind: SocketAddr = std::env::var("STEEL_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()?;

    let db = db::connect(&database_url).await?;
    let state = state::AppState::new(db);
    let app = api::router(state);

    tracing::info!("Steel is listening on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

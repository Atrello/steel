#![allow(clippy::result_large_err)]
mod api;
mod auth;
mod bots;
mod channels;
mod chat;
mod db;
mod dm;
mod events;
mod gateway;
mod members;
mod perms;
mod pins;
mod polls;
mod presence;
mod profile;
mod reactions;
mod roles;
mod search;
mod session;
mod state;
mod uploads;
mod voice;

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
    let voice_port = state.voice.config.port;
    let voice_socket = tokio::net::UdpSocket::bind(("0.0.0.0", voice_port)).await?;
    tracing::info!("Voice media on UDP {voice_port}");
    tokio::spawn(voice::udp::serve(voice_socket, state.voice.clone()));
    let app = api::router(state);

    tracing::info!("Steel is listening on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

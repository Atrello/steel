//! Gateway: JSON over WebSocket, no compression.
//! Hello -> Identify (new session, READY) or Resume (replay missed events) -> dispatches and heartbeats.
//! A client that cannot resume gets Invalid Session and identifies again.

use std::{pin::pin, sync::Arc, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use flint_protocol::{op, GatewayFrame, IdentifyData, PresenceUpdate, ResumeData, User};
use futures_util::StreamExt;
use serde_json::json;

use crate::{
    api, auth, chat, dm, presence,
    session::{self, Session, RESUME_WINDOW},
    state::AppState,
};

/// Client request to join, move or leave a voice channel.
const VOICE_STATE_UPDATE: u8 = 4;
pub const HEARTBEAT_INTERVAL_MS: u64 = 41_250;
/// Close the socket if no heartbeat arrives within this window.
const HEARTBEAT_GRACE: Duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS * 2);
/// Identify or Resume must arrive within this window.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(state: AppState, mut socket: WebSocket) {
    if send(&mut socket, &GatewayFrame::hello(HEARTBEAT_INTERVAL_MS))
        .await
        .is_err()
    {
        return;
    }

    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return;
    };
    let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else {
        return;
    };

    match frame.op {
        op::IDENTIFY => identify(&state, &mut socket, frame.d).await,
        op::RESUME => resume(&state, &mut socket, frame.d).await,
        _ => {}
    }
}

async fn identify(state: &AppState, socket: &mut WebSocket, data: serde_json::Value) {
    let Ok(data) = serde_json::from_value::<IdentifyData>(data) else {
        return;
    };
    let user = match api::user_from_token(state, &data.token).await {
        Ok(Some(user)) => user,
        _ => {
            close(socket, 4004, "Authentication failed.").await;
            return;
        }
    };

    let session = Session::new(auth::new_id(), user.id.clone());
    // Subscribe before loading READY data so no event between the two is lost.
    session::spawn_relay(session.clone(), state.events.subscribe());

    let (guilds, private_channels, read_state) = match load_ready_data(state, &user).await {
        Ok(data) => data,
        Err(e) => {
            tracing::error!("READY: could not load data: {e}");
            session.kill();
            close(socket, 4000, "Unknown error.").await;
            return;
        }
    };
    let ready = GatewayFrame::dispatch(
        1,
        "READY",
        json!({
            "v": 10,
            "user": user,
            "session_id": session.id,
            "resume_gateway_url": "/gateway",
            "guilds": guilds,
            "private_channels": private_channels,
            "read_state": read_state,
        }),
    );
    if send(socket, &ready).await.is_err() {
        session.kill();
        return;
    }
    state.sessions.insert(session.clone());
    serve(state, socket, user, session, 1).await;
}

async fn resume(state: &AppState, socket: &mut WebSocket, data: serde_json::Value) {
    let Ok(data) = serde_json::from_value::<ResumeData>(data) else {
        invalid_session(socket).await;
        return;
    };
    let user = match api::user_from_token(state, &data.token).await {
        Ok(Some(user)) => user,
        _ => {
            close(socket, 4004, "Authentication failed.").await;
            return;
        }
    };
    let Some(session) = state.sessions.get(&data.session_id) else {
        invalid_session(socket).await;
        return;
    };
    if session.user_id != user.id || session.is_dead() || !session.can_resume_from(data.seq) {
        state.sessions.remove(&session.id);
        invalid_session(socket).await;
        return;
    }
    let resumed = GatewayFrame::dispatch(data.seq, "RESUMED", json!({}));
    if send(socket, &resumed).await.is_err() {
        return;
    }
    serve(state, socket, user, session, data.seq).await;
}

/// Run one attached socket: replay buffered frames after `cursor`, deliver new ones, handle heartbeats
/// and presence, and detach the session when the socket ends.
async fn serve(
    state: &AppState,
    socket: &mut WebSocket,
    user: User,
    session: Arc<Session>,
    mut cursor: u64,
) {
    let gen = session.attach();
    if state.presence.connect(&user.id) {
        presence::broadcast_presence(state, &user.id).await;
    }

    let heartbeat_deadline = tokio::time::sleep(HEARTBEAT_GRACE);
    tokio::pin!(heartbeat_deadline);

    loop {
        // Enable the wait before draining, so a frame pushed in between wakes this loop.
        let mut notified = pin!(session.notify.notified());
        notified.as_mut().enable();

        if !session.is_current(gen) {
            break;
        }
        let Some(frames) = session.frames_after(cursor) else {
            break;
        };
        let mut failed = false;
        for frame in frames {
            cursor = frame.s.unwrap_or(cursor);
            if send(socket, &frame).await.is_err() {
                failed = true;
                break;
            }
        }
        if failed {
            break;
        }

        tokio::select! {
            _ = &mut notified => {}
            _ = &mut heartbeat_deadline => break,
            msg = socket.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else { break };
                        match frame.op {
                            op::HEARTBEAT => {
                                heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_GRACE);
                                if send(socket, &GatewayFrame::ack()).await.is_err() {
                                    break;
                                }
                            }
                            op::PRESENCE_UPDATE => {
                                if let Ok(update) = serde_json::from_value::<PresenceUpdate>(frame.d) {
                                    state.presence.set_status(&user.id, &update.status);
                                    presence::broadcast_presence(state, &user.id).await;
                                }
                            }
                            VOICE_STATE_UPDATE => {
                                crate::voice::gateway_state(state, &user, &session.id, frame.d).await;
                            }
                            _ => {}
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    // Keep the session for a while so the client can resume. Expire it if nothing reattaches.
    if session.detach(gen) {
        let state = state.clone();
        let session = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESUME_WINDOW).await;
            if session.still_detached_since(gen) {
                state.sessions.remove(&session.id);
            }
        });
    }
    if state.presence.disconnect(&user.id) {
        presence::broadcast_presence(state, &user.id).await;
    }
}

async fn load_ready_data(
    state: &AppState,
    user: &User,
) -> Result<
    (
        Vec<flint_protocol::Guild>,
        Vec<flint_protocol::Channel>,
        Vec<flint_protocol::ReadState>,
    ),
    sqlx::Error,
> {
    let guilds = chat::guilds_for_user(state, &user.id).await?;
    let private_channels = dm::private_channels_for_user(state, &user.id).await?;
    let read_state = presence::read_states_for_user(state, &user.id).await?;
    Ok((guilds, private_channels, read_state))
}

async fn invalid_session(socket: &mut WebSocket) {
    let _ = send(
        socket,
        &GatewayFrame {
            op: op::INVALID_SESSION,
            d: json!(false),
            s: None,
            t: None,
        },
    )
    .await;
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn send(socket: &mut WebSocket, frame: &GatewayFrame) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}

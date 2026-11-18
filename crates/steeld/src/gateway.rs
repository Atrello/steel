//! Gateway: JSON over WebSocket, no compression.
//! Hello -> Identify (or Resume) -> READY, then Heartbeat/ACK and event dispatch.
//! Resume is not implemented yet; a resume attempt gets Invalid Session.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use flint_protocol::{op, GatewayFrame, IdentifyData, User};
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::{api, chat, events::Dispatch, state::AppState};

pub const HEARTBEAT_INTERVAL_MS: u64 = 41_250;
/// Close the socket if no heartbeat arrives within this window.
const HEARTBEAT_GRACE: Duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS * 2);
/// Identify must arrive within this window.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(state: AppState, mut socket: WebSocket) {
    if send(&mut socket, &GatewayFrame::hello(HEARTBEAT_INTERVAL_MS)).await.is_err() {
        return;
    }

    let first = tokio::time::timeout(IDENTIFY_TIMEOUT, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return;
    };
    let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else {
        return;
    };

    let user = match frame.op {
        op::IDENTIFY => {
            let Ok(data) = serde_json::from_value::<IdentifyData>(frame.d) else {
                return;
            };
            match api::user_from_token(&state, &data.token).await {
                Ok(Some(user)) => user,
                _ => {
                    close(&mut socket, 4004, "Authentication failed.").await;
                    return;
                }
            }
        }
        // Resume needs stored session state. Ask the client to identify again.
        op::RESUME => {
            let _ = send(&mut socket, &GatewayFrame { op: op::INVALID_SESSION, d: json!(false), s: None, t: None }).await;
            return;
        }
        _ => return,
    };

    // Subscribe before building READY so no dispatch between the two is lost.
    let mut events = state.events.subscribe();
    let session_id = crate::auth::new_id();
    let guilds = match chat::guilds_for_user(&state, &user.id).await {
        Ok(g) => g,
        Err(e) => {
            tracing::error!("READY: could not load guilds: {e}");
            close(&mut socket, 4000, "Unknown error.").await;
            return;
        }
    };
    let mut seq: u64 = 1;
    let ready = GatewayFrame::dispatch(
        seq,
        "READY",
        json!({
            "v": 10,
            "user": user,
            "session_id": session_id,
            "resume_gateway_url": "/gateway",
            "guilds": guilds,
            "private_channels": [],
        }),
    );
    if send(&mut socket, &ready).await.is_err() {
        return;
    }

    let heartbeat_deadline = tokio::time::sleep(HEARTBEAT_GRACE);
    tokio::pin!(heartbeat_deadline);

    loop {
        tokio::select! {
            _ = &mut heartbeat_deadline => break,
            dispatch = events.recv() => {
                match dispatch {
                    Ok(dispatch) => {
                        if !deliver(&mut socket, &user, dispatch, &mut seq).await {
                            break;
                        }
                    }
                    // A slow reader missed events. Drop the connection so the client resyncs on reconnect.
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("gateway session lagged by {n} events; closing");
                        break;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            msg = socket.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else { break };
                        if frame.op == op::HEARTBEAT {
                            heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_GRACE);
                            if send(&mut socket, &GatewayFrame::ack()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Send a dispatch if this user is a target. Returns false when the socket is gone.
async fn deliver(socket: &mut WebSocket, user: &User, dispatch: Dispatch, seq: &mut u64) -> bool {
    if !dispatch.targets.iter().any(|t| *t == user.id) {
        return true;
    }
    *seq += 1;
    let frame = GatewayFrame::dispatch(*seq, dispatch.event, dispatch.data);
    send(socket, &frame).await.is_ok()
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame { code, reason: reason.into() })))
        .await;
}

async fn send(socket: &mut WebSocket, frame: &GatewayFrame) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}

//! Gateway: JSON over WebSocket, no compression.
//! Hello -> Identify (or Resume) -> READY/RESUMED, then Heartbeat/ACK. Phase 1 has no event fan-out yet.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use flint_protocol::{op, GatewayFrame, IdentifyData};
use futures_util::StreamExt;
use serde_json::json;

use crate::{api, state::AppState};

pub const HEARTBEAT_INTERVAL_MS: u64 = 41_250;
/// Close the socket if no heartbeat arrives within this window after Hello.
const HEARTBEAT_GRACE: Duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS * 2);
/// Identify must arrive within this window.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(state: AppState, mut socket: WebSocket) {
    if send(&mut socket, &GatewayFrame::hello(HEARTBEAT_INTERVAL_MS))
        .await
        .is_err()
    {
        return;
    }

    let mut seq: u64 = 0;

    let first = tokio::time::timeout(IDENTIFY_TIMEOUT, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return;
    };
    let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else {
        return;
    };

    match frame.op {
        op::IDENTIFY => {
            let Ok(data) = serde_json::from_value::<IdentifyData>(frame.d) else {
                return;
            };
            match api::user_from_token(&state, &data.token).await {
                Ok(Some(user)) => {
                    let session_id = crate::auth::new_id();
                    seq += 1;
                    let ready = GatewayFrame::dispatch(
                        seq,
                        "READY",
                        json!({
                            "v": 10,
                            "user": user,
                            "session_id": session_id,
                            "resume_gateway_url": "/gateway",
                            "guilds": [],
                            "private_channels": [],
                        }),
                    );
                    if send(&mut socket, &ready).await.is_err() {
                        return;
                    }
                }
                // Invalid token: close with 4004 (Authentication failed), as the Discord gateway does.
                _ => {
                    let _ = socket
                        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 4004,
                            reason: "Authentication failed.".into(),
                        })))
                        .await;
                    return;
                }
            }
        }
        // Resume needs stored session state, which lands with event fan-out. Ask the client to re-identify.
        op::RESUME => {
            let _ = send(
                &mut socket,
                &GatewayFrame {
                    op: op::INVALID_SESSION,
                    d: json!(false),
                    s: None,
                    t: None,
                },
            )
            .await;
            return;
        }
        _ => return,
    }

    let heartbeat_deadline = tokio::time::sleep(HEARTBEAT_GRACE);
    tokio::pin!(heartbeat_deadline);

    loop {
        tokio::select! {
            _ = &mut heartbeat_deadline => break,
            msg = socket.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<GatewayFrame>(&text) else { break };
                        match frame.op {
                            op::HEARTBEAT => {
                                heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_GRACE);
                                if send(&mut socket, &GatewayFrame::ack()).await.is_err() {
                                    break;
                                }
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
}

async fn send(socket: &mut WebSocket, frame: &GatewayFrame) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).unwrap_or_default();
    socket.send(Message::Text(text.into())).await
}

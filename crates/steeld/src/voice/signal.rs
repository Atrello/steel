//! The voice WebSocket (Discord voice gateway v8, JSON). A client identifies with the token from
//! VOICE_SERVER_UPDATE, receives its SSRC and the UDP address, selects the transport mode, and
//! gets the call's transport key. Speaking and membership events fan out to the call.

use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::{CallEvent, Member, MODE};
use crate::state::AppState;

/// Heartbeat interval sent in Hello (ms). Clients may choose a shorter one, capped at 5000 ms.
const HEARTBEAT_MS: u64 = 5000;
/// Close the signaling socket when the client goes silent for this many heartbeat intervals.
const HEARTBEAT_GRACE_MULTIPLE: u32 = 3;

mod op {
    pub const IDENTIFY: u64 = 0;
    pub const SELECT_PROTOCOL: u64 = 1;
    pub const READY: u64 = 2;
    pub const HEARTBEAT: u64 = 3;
    pub const SESSION_DESCRIPTION: u64 = 4;
    pub const SPEAKING: u64 = 5;
    pub const HEARTBEAT_ACK: u64 = 6;
    pub const HELLO: u64 = 8;
    pub const CLIENTS_CONNECT: u64 = 11;
}

fn frame(op: u64, d: Value) -> String {
    json!({ "op": op, "d": d }).to_string()
}

/// Run one voice signaling socket until it closes or the membership ends.
pub async fn run_ws(state: AppState, mut socket: WebSocket) {
    if socket
        .send(Message::Text(frame(op::HELLO, json!({ "heartbeat_interval": HEARTBEAT_MS })).into()))
        .await
        .is_err()
    {
        return;
    }

    let Some(identify) = next_json(&mut socket).await else { return };
    if identify["op"].as_u64() != Some(op::IDENTIFY) {
        return;
    }
    let d = &identify["d"];
    let (Some(user_id), Some(session_id), Some(token)) = (
        d["user_id"].as_str(),
        d["session_id"].as_str(),
        d["token"].as_str(),
    ) else {
        close(&mut socket, 4006, "Invalid session.").await;
        return;
    };
    let Some(attached) = state.voice.attach(token, user_id, session_id) else {
        close(&mut socket, 4006, "Invalid session.").await;
        return;
    };
    let member = attached.member.clone();
    let channel_id = member.channel_id.clone();
    let mut events = attached.events;

    let ready = frame(
        op::READY,
        json!({
            "ssrc": member.ssrc,
            "ip": state.voice.config.advertise_ip,
            "port": state.voice.config.port,
            "modes": [MODE],
            "heartbeat_interval": HEARTBEAT_MS,
        }),
    );
    if socket.send(Message::Text(ready.into())).await.is_err() {
        end_membership(&state, &member, &channel_id);
        return;
    }
    let roster = frame(op::CLIENTS_CONNECT, json!({ "user_ids": attached.roster }));
    if socket.send(Message::Text(roster.into())).await.is_err() {
        end_membership(&state, &member, &channel_id);
        return;
    }

    let grace = std::time::Duration::from_millis(HEARTBEAT_MS * u64::from(HEARTBEAT_GRACE_MULTIPLE));
    let heartbeat_deadline = tokio::time::sleep(grace);
    tokio::pin!(heartbeat_deadline);
    // Created before the loop so a membership end cannot be missed between iterations.
    let ended = member.ended.notified();
    tokio::pin!(ended);

    loop {
        tokio::select! {
            _ = &mut ended => break,
            _ = &mut heartbeat_deadline => break,
            received = events.recv() => match received {
                Ok(CallEvent { exclude, text }) => {
                    if exclude != member.user_id
                        && socket.send(Message::Text(text.into())).await.is_err()
                    {
                        break;
                    }
                }
                // A lagging socket missed signaling it cannot reconstruct, so it must reconnect.
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => break,
            },
            msg = socket.next() => {
                let Some(Ok(msg)) = msg else { break };
                let text = match msg {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    // DAVE binary signaling is not handled yet.
                    _ => continue,
                };
                let Ok(event) = serde_json::from_str::<Value>(&text) else { break };
                let op = event["op"].as_u64().unwrap_or(u64::MAX);
                let d = &event["d"];
                match op {
                    op::SELECT_PROTOCOL => {
                        if d["protocol"].as_str() != Some("udp") {
                            continue;
                        }
                        let key: Vec<u8> = attached.key.to_vec();
                        let reply = frame(
                            op::SESSION_DESCRIPTION,
                            json!({
                                "mode": MODE,
                                "secret_key": key,
                                "dave_protocol_version": 1,
                            }),
                        );
                        if socket.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                    op::HEARTBEAT => {
                        heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + grace);
                        let ack = frame(op::HEARTBEAT_ACK, json!({ "t": d["t"] }));
                        if socket.send(Message::Text(ack.into())).await.is_err() {
                            break;
                        }
                    }
                    op::SPEAKING => {
                        let out = frame(
                            op::SPEAKING,
                            json!({
                                "speaking": d["speaking"],
                                "delay": d["delay"],
                                "ssrc": member.ssrc,
                                "user_id": member.user_id,
                            }),
                        );
                        state.voice.broadcast(&channel_id, &member.user_id, out);
                    }
                    _ => {}
                }
            }
        }
    }

    end_membership(&state, &member, &channel_id);
}

/// End the membership this socket belongs to, unless it has already been replaced.
fn end_membership(state: &AppState, member: &Member, channel_id: &str) {
    state
        .voice
        .leave_if_current(channel_id, &member.user_id, &member.token);
}

async fn next_json(socket: &mut WebSocket) -> Option<Value> {
    let timeout = std::time::Duration::from_secs(30);
    let msg = tokio::time::timeout(timeout, socket.next()).await.ok()??.ok()?;
    match msg {
        Message::Text(text) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}


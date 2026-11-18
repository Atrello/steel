//! Signaling for peer-to-peer calls and screen share.
//!
//! Steel only relays WebRTC session descriptions and ICE candidates between two users who
//! share a DM or are friends. It stores nothing, and audio, video and screen frames never
//! touch the server. Clients take their ICE server list from `GET /calls/ice-servers`.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use flint_protocol::relationship_type;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    api::{API_PREFIX, api_error, user_from_headers},
    db,
    dm::is_blocked_either_way,
    state::AppState,
};

/// Largest signaling payload accepted, in bytes of serialized JSON. SDP is a few KiB.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const KINDS: [&str; 5] = ["offer", "answer", "ice", "hangup", "decline"];

#[derive(Deserialize)]
pub struct SignalRequest {
    pub to: String,
    pub call_id: String,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(&format!("{API_PREFIX}/calls/signal"), post(signal))
        .route(&format!("{API_PREFIX}/calls/ice-servers"), get(ice_servers))
}

fn db_err(e: sqlx::Error) -> Response {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, 0, e.to_string())
}

fn valid_call_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Two users may signal when they are friends or share a DM or group DM.
async fn may_signal(state: &AppState, me: &str, other: &str) -> Result<bool, sqlx::Error> {
    let friend: Option<(i64,)> = sqlx::query_as(&db::sql(
        "SELECT 1 FROM relationships WHERE user_id = ? AND target_id = ? AND kind = ?",
    ))
    .bind(me)
    .bind(other)
    .bind(relationship_type::FRIEND as i64)
    .fetch_optional(&state.db)
    .await?;
    if friend.is_some() {
        return Ok(true);
    }
    let shared: Option<(i64,)> = sqlx::query_as(&db::sql(
        "SELECT 1 FROM channel_recipients a JOIN channel_recipients b ON a.channel_id = b.channel_id \
         WHERE a.user_id = ? AND b.user_id = ? LIMIT 1",
    ))
    .bind(me)
    .bind(other)
    .fetch_optional(&state.db)
    .await?;
    Ok(shared.is_some())
}

/// `POST /calls/signal`: relay one offer, answer, ICE candidate, hangup or decline to `to`.
async fn signal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SignalRequest>,
) -> Response {
    let me = match user_from_headers(&state, &headers).await {
        Ok(Some(user)) => user,
        Ok(None) => return api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"),
        Err(e) => return db_err(e),
    };
    if !KINDS.contains(&req.kind.as_str()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "kind must be offer, answer, ice, hangup or decline",
        );
    }
    if !valid_call_id(&req.call_id) {
        return api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "call_id must be 1-64 letters, digits or dashes",
        );
    }
    if req.to == me.id {
        return api_error(StatusCode::BAD_REQUEST, 50035, "You cannot call yourself");
    }
    if serde_json::to_vec(&req.payload)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
        > MAX_PAYLOAD_BYTES
    {
        return api_error(StatusCode::PAYLOAD_TOO_LARGE, 50035, "payload is too large");
    }
    match is_blocked_either_way(&state, &me.id, &req.to).await {
        Ok(false) => {}
        Ok(true) => return api_error(StatusCode::FORBIDDEN, 40002, "Cannot call this user"),
        Err(e) => return db_err(e),
    }
    match may_signal(&state, &me.id, &req.to).await {
        Ok(true) => {}
        Ok(false) => {
            return api_error(
                StatusCode::FORBIDDEN,
                50001,
                "You can only call friends or people in a shared DM",
            );
        }
        Err(e) => return db_err(e),
    }
    state.publish(
        vec![req.to.clone()],
        "CALL_SIGNAL",
        json!({
            "from": { "id": me.id, "username": me.username },
            "call_id": req.call_id,
            "kind": req.kind,
            "payload": req.payload,
        }),
    );
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /calls/ice-servers`: STUN and optional TURN servers for WebRTC.
///
/// Env: `STEEL_STUN_URLS` (comma list, default Google's public STUN), `STEEL_TURN_URLS`
/// (comma list), `STEEL_TURN_USERNAME`, `STEEL_TURN_CREDENTIAL`. The TURN credential is a
/// static secret handed to every signed-in user; use short-lived credentials in production.
async fn ice_servers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match user_from_headers(&state, &headers).await {
        Ok(Some(_)) => {}
        Ok(None) => return api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"),
        Err(e) => return db_err(e),
    }
    Json(ice_server_config()).into_response()
}

fn split_env(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn ice_server_config() -> Value {
    let stun = split_env("STEEL_STUN_URLS");
    let stun = if stun.is_empty() {
        vec!["stun:stun.l.google.com:19302".to_string()]
    } else {
        stun
    };
    let mut servers = vec![json!({ "urls": stun })];
    let turn = split_env("STEEL_TURN_URLS");
    if !turn.is_empty() {
        servers.push(json!({
            "urls": turn,
            "username": std::env::var("STEEL_TURN_USERNAME").unwrap_or_default(),
            "credential": std::env::var("STEEL_TURN_CREDENTIAL").unwrap_or_default(),
        }));
    }
    json!({ "iceServers": servers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_ids_are_tokens() {
        assert!(valid_call_id("a1-b2"));
        assert!(!valid_call_id(""));
        assert!(!valid_call_id("a b"));
        assert!(!valid_call_id(&"x".repeat(65)));
    }
}

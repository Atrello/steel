//! Presence (online status) and read state.
//!
//! Presence lives in memory: a user is online while at least one gateway session is connected.
//! Status changes are sent to people who share a guild, a DM or a friendship with the user.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use flint_protocol::{status, ReadState};
use serde_json::json;

use crate::{
    api::{api_error, user_from_headers, API_PREFIX},
    state::AppState,
};

#[derive(Clone)]
struct Session {
    connections: u32,
    status: String,
}

#[derive(Default)]
pub struct PresenceTracker {
    sessions: Mutex<HashMap<String, Session>>,
}

impl PresenceTracker {
    /// Register a new gateway connection. Returns true when this is the user's first connection.
    pub fn connect(&self, user_id: &str) -> bool {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(user_id.to_string()).or_insert(Session { connections: 0, status: status::ONLINE.into() });
        entry.connections += 1;
        entry.connections == 1
    }

    /// Drop a gateway connection. Returns true when it was the user's last one.
    pub fn disconnect(&self, user_id: &str) -> bool {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(user_id) {
            Some(entry) if entry.connections > 1 => {
                entry.connections -= 1;
                false
            }
            Some(_) => {
                map.remove(user_id);
                true
            }
            None => false,
        }
    }

    pub fn set_status(&self, user_id: &str, new_status: &str) {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(user_id) {
            entry.status = new_status.to_string();
        }
    }

    /// Status as other users see it. Invisible users appear offline.
    pub fn visible_status(&self, user_id: &str) -> String {
        let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(user_id) {
            Some(s) if s.status == status::INVISIBLE => status::OFFLINE.into(),
            Some(s) => s.status.clone(),
            None => status::OFFLINE.into(),
        }
    }
}

/// Users who should hear about `user_id`'s presence: guild co-members, DM peers and friends.
pub async fn presence_audience(state: &AppState, user_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT other FROM ( \
            SELECT m2.user_id AS other FROM guild_members m1 JOIN guild_members m2 ON m2.guild_id = m1.guild_id WHERE m1.user_id = ? \
            UNION \
            SELECT r2.user_id AS other FROM channel_recipients r1 JOIN channel_recipients r2 ON r2.channel_id = r1.channel_id WHERE r1.user_id = ? \
            UNION \
            SELECT target_id AS other FROM relationships WHERE user_id = ? AND kind = 1 \
         ) WHERE other <> ?",
    )
    .bind(user_id).bind(user_id).bind(user_id).bind(user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub fn presence_event(user_id: &str, status: &str) -> serde_json::Value {
    json!({
        "user": { "id": user_id },
        "status": status,
        "activities": [],
        "client_status": {},
    })
}

/// Tell everyone who shares a relationship with `user_id` that its status changed.
pub async fn broadcast_presence(state: &AppState, user_id: &str) {
    let status = state.presence.visible_status(user_id);
    match presence_audience(state, user_id).await {
        Ok(targets) if !targets.is_empty() => {
            state.publish(targets, "PRESENCE_UPDATE", presence_event(user_id, &status));
        }
        Ok(_) => {}
        Err(e) => tracing::error!("presence audience lookup failed: {e}"),
    }
}

/// Read state for the user's channels, sent in READY.
pub async fn read_states_for_user(state: &AppState, user_id: &str) -> Result<Vec<ReadState>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT channel_id, last_message_id FROM read_states WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(&state.db)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, last)| ReadState { id, last_message_id: last, mention_count: 0 })
        .collect())
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/ack"),
        post(ack_message),
    )
}

/// Mark a message (and everything before it) as read in a channel.
async fn ack_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let user = match user_from_headers(&state, &headers).await {
        Ok(Some(u)) => u,
        Ok(None) => return api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"),
        Err(e) => return crate::api::internal(e),
    };
    let channel = match crate::chat::channel_row(&state, &channel_id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(resp) = crate::chat::require_channel(&state, &channel, &user.id).await {
        return resp;
    }
    // Only move the read marker forward.
    let update = sqlx::query(
        "INSERT INTO read_states (user_id, channel_id, last_message_id) VALUES (?, ?, ?) \
         ON CONFLICT(user_id, channel_id) DO UPDATE SET last_message_id = excluded.last_message_id \
         WHERE CAST(excluded.last_message_id AS INTEGER) > CAST(COALESCE(read_states.last_message_id, '0') AS INTEGER)",
    )
    .bind(&user.id)
    .bind(&channel_id)
    .bind(&message_id)
    .execute(&state.db)
    .await;
    if let Err(e) = update {
        return crate::api::internal(e);
    }
    state.publish(
        vec![user.id.clone()],
        "MESSAGE_ACK",
        json!({ "channel_id": channel_id, "message_id": message_id, "mention_count": 0 }),
    );
    (StatusCode::OK, axum::Json(json!({ "token": null }))).into_response()
}

//! Pinned messages. In guilds only the owner pins or unpins; DM participants can pin in their DM.
//! A channel holds at most 50 pins, as Discord allows.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use flint_protocol::Message;
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::{api_error, API_PREFIX},
    chat::{self, ApiResult, MESSAGE_SELECT},
    state::AppState,
};

const MAX_PINS: i64 = 50;
const PAGE_LIMIT: u32 = 25;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/pins"),
            get(list_pins),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/pins/{{message_id}}"),
            axum::routing::put(pin).delete(unpin),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

/// Who may pin: the guild owner in guilds, any participant in a DM.
async fn may_pin(state: &AppState, channel: &chat::ChannelRow, user_id: &str) -> ApiResult<()> {
    match &channel.guild_id {
        Some(guild_id) => {
            if chat::guild_row(state, guild_id).await?.owner_id != user_id {
                return Err(api_error(
                    StatusCode::FORBIDDEN,
                    50013,
                    "Missing Permissions",
                ));
            }
            Ok(())
        }
        None => Ok(()),
    }
}

async fn publish_pins(state: &AppState, channel: &chat::ChannelRow) -> ApiResult<()> {
    let last: Option<(Option<String>,)> =
        sqlx::query_as("SELECT MAX(pinned_at) FROM pins WHERE channel_id = ?")
            .bind(&channel.id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
    let targets = chat::channel_audience(state, channel).await.map_err(db)?;
    state.publish(
        targets,
        "CHANNEL_PINS_UPDATE",
        json!({
            "channel_id": channel.id,
            "guild_id": channel.guild_id,
            "last_pin_timestamp": last.and_then(|(t,)| t),
        }),
    );
    Ok(())
}

async fn pin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        may_pin(&state, &channel, &user.id).await?;
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM messages WHERE id = ? AND channel_id = ?")
            .bind(&message_id)
            .bind(&channel_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
        if exists.is_none() {
            return Err(chat::missing_message());
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins WHERE channel_id = ?")
            .bind(&channel_id)
            .fetch_one(&state.db)
            .await
            .map_err(db)?;
        let already: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM pins WHERE channel_id = ? AND message_id = ?")
            .bind(&channel_id)
            .bind(&message_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
        if already.is_none() && count >= MAX_PINS {
            return Err(api_error(StatusCode::BAD_REQUEST, 30003, "maximum number of pinned messages reached"));
        }
        sqlx::query("INSERT OR IGNORE INTO pins (channel_id, message_id, pinned_at, pinned_by) VALUES (?, ?, ?, ?)")
            .bind(&channel_id)
            .bind(&message_id)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&user.id)
            .execute(&state.db)
            .await
            .map_err(db)?;
        publish_pins(&state, &channel).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn unpin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        may_pin(&state, &channel, &user.id).await?;
        sqlx::query("DELETE FROM pins WHERE channel_id = ? AND message_id = ?")
            .bind(&channel_id)
            .bind(&message_id)
            .execute(&state.db)
            .await
            .map_err(db)?;
        publish_pins(&state, &channel).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

#[derive(Deserialize)]
pub struct PinsQuery {
    pub limit: Option<u32>,
    /// Only pins made before this ISO time (the `pinned_at` of the last item on the previous page).
    pub before: Option<String>,
}

#[derive(serde::Serialize)]
struct PinItem {
    pinned_at: String,
    message: Message,
}

#[derive(serde::Serialize)]
struct PinPage {
    items: Vec<PinItem>,
    has_more: bool,
}

/// `GET /channels/{id}/messages/pins`: newest pins first, paged by `before`.
async fn list_pins(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Query(q): Query<PinsQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        let limit = q.limit.unwrap_or(PAGE_LIMIT).clamp(1, PAGE_LIMIT) as i64;
        let rows: Vec<(String, String)> = match q.before {
            Some(before) => sqlx::query_as(
                "SELECT pinned_at, message_id FROM pins WHERE channel_id = ? AND pinned_at < ? \
                 ORDER BY pinned_at DESC, message_id DESC LIMIT ?",
            )
            .bind(&channel_id)
            .bind(before)
            .bind(limit + 1)
            .fetch_all(&state.db)
            .await
            .map_err(db)?,
            None => sqlx::query_as(
                "SELECT pinned_at, message_id FROM pins WHERE channel_id = ? ORDER BY pinned_at DESC, message_id DESC LIMIT ?",
            )
            .bind(&channel_id)
            .bind(limit + 1)
            .fetch_all(&state.db)
            .await
            .map_err(db)?,
        };
        let has_more = rows.len() as i64 > limit;
        let rows: Vec<(String, String)> = rows.into_iter().take(limit as usize).collect();
        let mut messages = Vec::with_capacity(rows.len());
        for (_, message_id) in &rows {
            let row: chat::MessageRow = sqlx::query_as(&format!("{MESSAGE_SELECT} WHERE m.id = ?"))
                .bind(message_id)
                .fetch_one(&state.db)
                .await
                .map_err(db)?;
            messages.push(chat::message_json(row));
        }
        crate::uploads::hydrate(&state, &mut messages).await.map_err(db)?;
        crate::reactions::hydrate(&state, &user.id, &mut messages).await.map_err(db)?;
        crate::polls::hydrate(&state, &user.id, &mut messages).await.map_err(db)?;
        let items = rows
            .into_iter()
            .zip(messages)
            .map(|((pinned_at, _), message)| PinItem { pinned_at, message })
            .collect();
        Ok(Json(PinPage { items, has_more }).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

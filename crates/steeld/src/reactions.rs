//! Message reactions. An emoji is stored as the string the client sends in the path: a Unicode character,
//! or `name:id` for a custom emoji. Each user can react with each emoji once.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use flint_protocol::{Message, Reaction, ReactionEmoji};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::{api_error, API_PREFIX},
    chat::{self, ApiResult},
    state::AppState,
};

/// Distinct emoji per message, as Discord allows.
const MAX_REACTIONS_PER_MESSAGE: i64 = 20;
const MAX_EMOJI_LEN: usize = 64;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/reactions/{{emoji}}/@me"),
            axum::routing::put(add_own).delete(remove_own),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/reactions/{{emoji}}/{{user_id}}"),
            delete(remove_user),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/reactions/{{emoji}}"),
            get(list_users),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/reactions"),
            delete(clear_all),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

/// Emoji path parameter to the stored key, validated.
pub fn emoji_key(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_EMOJI_LEN || trimmed.chars().any(char::is_control)
    {
        return Err(api_error(StatusCode::BAD_REQUEST, 10014, "Unknown Emoji"));
    }
    Ok(trimmed.to_string())
}

/// Emoji key to the object clients receive: `name:id` splits into name and id.
pub fn emoji_json(key: &str) -> ReactionEmoji {
    match key.rsplit_once(':') {
        Some((name, id)) if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => {
            ReactionEmoji {
                id: Some(id.to_string()),
                name: Some(name.to_string()),
            }
        }
        _ => ReactionEmoji {
            id: None,
            name: Some(key.to_string()),
        },
    }
}

/// The message must exist in this channel, and the caller must be able to see the channel.
async fn message_in_channel(
    state: &AppState,
    headers: &HeaderMap,
    channel_id: &str,
    message_id: &str,
) -> ApiResult<(String, chat::ChannelRow)> {
    let user = chat::authed(state, headers).await?;
    let channel = chat::channel_row(state, channel_id).await?;
    chat::require_channel(state, &channel, &user.id).await?;
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM messages WHERE id = ? AND channel_id = ?")
            .bind(message_id)
            .bind(channel_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
    if exists.is_none() {
        return Err(chat::missing_message());
    }
    Ok((user.id, channel))
}

async fn publish_to_channel(
    state: &AppState,
    channel: &chat::ChannelRow,
    event: &'static str,
    data: serde_json::Value,
) -> ApiResult<()> {
    let targets = chat::channel_audience(state, channel).await.map_err(db)?;
    state.publish(targets, event, data);
    Ok(())
}

async fn add_own(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let key = emoji_key(&emoji)?;
        let (user_id, channel) = message_in_channel(&state, &headers, &channel_id, &message_id).await?;
        let distinct: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT emoji) FROM reactions WHERE message_id = ?")
            .bind(&message_id)
            .fetch_one(&state.db)
            .await
            .map_err(db)?;
        let already: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM reactions WHERE message_id = ? AND emoji = ?")
            .bind(&message_id)
            .bind(&key)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
        if already.is_none() && distinct >= MAX_REACTIONS_PER_MESSAGE {
            return Err(api_error(StatusCode::BAD_REQUEST, 90001, "too many different reactions on this message"));
        }
        sqlx::query("INSERT OR IGNORE INTO reactions (message_id, user_id, emoji, created_at) VALUES (?, ?, ?, ?)")
            .bind(&message_id)
            .bind(&user_id)
            .bind(&key)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&state.db)
            .await
            .map_err(db)?;
        let guild_id = channel.guild_id.clone();
        publish_to_channel(&state, &channel, "MESSAGE_REACTION_ADD", json!({
            "user_id": user_id, "channel_id": channel_id, "message_id": message_id,
            "guild_id": guild_id, "emoji": emoji_json(&key),
        }))
        .await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn remove_own(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let key = emoji_key(&emoji)?;
        let (user_id, channel) =
            message_in_channel(&state, &headers, &channel_id, &message_id).await?;
        sqlx::query("DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji = ?")
            .bind(&message_id)
            .bind(&user_id)
            .bind(&key)
            .execute(&state.db)
            .await
            .map_err(db)?;
        publish_to_channel(
            &state,
            &channel,
            "MESSAGE_REACTION_REMOVE",
            json!({
                "user_id": user_id, "channel_id": channel_id, "message_id": message_id,
                "guild_id": channel.guild_id, "emoji": emoji_json(&key),
            }),
        )
        .await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Remove someone else's reaction. Guild owners may do this; a DM participant may not.
async fn remove_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id, emoji, target)): Path<(String, String, String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let key = emoji_key(&emoji)?;
        let (me, channel) = message_in_channel(&state, &headers, &channel_id, &message_id).await?;
        let is_owner = match &channel.guild_id {
            Some(guild_id) => chat::guild_row(&state, guild_id).await?.owner_id == me,
            None => false,
        };
        if !is_owner {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                50013,
                "Missing Permissions",
            ));
        }
        sqlx::query("DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji = ?")
            .bind(&message_id)
            .bind(&target)
            .bind(&key)
            .execute(&state.db)
            .await
            .map_err(db)?;
        publish_to_channel(
            &state,
            &channel,
            "MESSAGE_REACTION_REMOVE",
            json!({
                "user_id": target, "channel_id": channel_id, "message_id": message_id,
                "guild_id": channel.guild_id, "emoji": emoji_json(&key),
            }),
        )
        .await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Remove every reaction on a message (guild owner only).
async fn clear_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> =
        async {
            let (me, channel) =
                message_in_channel(&state, &headers, &channel_id, &message_id).await?;
            let is_owner = match &channel.guild_id {
                Some(guild_id) => chat::guild_row(&state, guild_id).await?.owner_id == me,
                None => false,
            };
            if !is_owner {
                return Err(api_error(
                    StatusCode::FORBIDDEN,
                    50013,
                    "Missing Permissions",
                ));
            }
            sqlx::query("DELETE FROM reactions WHERE message_id = ?")
                .bind(&message_id)
                .execute(&state.db)
                .await
                .map_err(db)?;
            publish_to_channel(&state, &channel, "MESSAGE_REACTION_REMOVE_ALL", json!({
            "channel_id": channel_id, "message_id": message_id, "guild_id": channel.guild_id,
        }))
        .await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        .await;
    result.unwrap_or_else(|e| e)
}

#[derive(Deserialize)]
pub struct UsersQuery {
    pub limit: Option<u32>,
}

/// Users who reacted with this emoji, oldest first.
async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    Query(q): Query<UsersQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let key = emoji_key(&emoji)?;
        message_in_channel(&state, &headers, &channel_id, &message_id).await?;
        let limit = q.limit.unwrap_or(25).clamp(1, 100);
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT u.id, u.username FROM reactions r JOIN users u ON u.id = r.user_id \
             WHERE r.message_id = ? AND r.emoji = ? ORDER BY r.rowid LIMIT ?",
        )
        .bind(&message_id)
        .bind(&key)
        .bind(limit)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        let users: Vec<_> = rows
            .into_iter()
            .map(|(id, name)| chat::plain_user(id, name))
            .collect();
        Ok(Json(users).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Fill in `reactions` on messages, from the point of view of `me`.
pub async fn hydrate(
    state: &AppState,
    me: &str,
    messages: &mut [Message],
) -> Result<(), sqlx::Error> {
    if messages.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; messages.len()].join(",");
    let sql = format!(
        "SELECT message_id, emoji, COUNT(*), MAX(CASE WHEN user_id = ? THEN 1 ELSE 0 END) FROM reactions \
         WHERE message_id IN ({placeholders}) GROUP BY message_id, emoji ORDER BY MIN(rowid)"
    );
    let mut q = sqlx::query_as::<_, (String, String, i64, i64)>(&sql).bind(me);
    for m in messages.iter() {
        q = q.bind(m.id.clone());
    }
    for (message_id, emoji, count, me_flag) in q.fetch_all(&state.db).await? {
        if let Some(m) = messages.iter_mut().find(|m| m.id == message_id) {
            m.reactions.push(Reaction {
                emoji: emoji_json(&emoji),
                count: count as u32,
                me: me_flag == 1,
            });
        }
    }
    Ok(())
}

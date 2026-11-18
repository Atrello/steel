//! Guild lifecycle and channel management: edit, delete, reorder, and invite listing.
//! Routes are wired on the existing paths in `chat::routes`, since axum allows one router per path.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use flint_protocol::{channel_type, Channel, Invite};
use serde_json::Value;
use sqlx::Row;

use crate::{
    api::api_error,
    chat::{self, ApiResult},
    state::AppState,
    uploads,
};

const BAD_BODY: u32 = 50035;

fn bad(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, BAD_BODY, message)
}

fn ok_json<T: serde::Serialize>(value: &T) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

/// Owner-only guild management: Phase 1 rule.
async fn owned_guild(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<chat::GuildRow> {
    let guild = chat::guild_row(state, guild_id).await?;
    if guild.owner_id != user_id {
        return Err(chat::no_access());
    }
    Ok(guild)
}

/// A parent must be a category in the same guild.
pub async fn check_parent(state: &AppState, guild_id: &str, parent: &str) -> ApiResult<()> {
    let row = chat::channel_row(state, parent).await?;
    if row.guild_id.as_deref() != Some(guild_id) || row.kind != channel_type::GUILD_CATEGORY as i64
    {
        return Err(bad("parent must be a category in this guild"));
    }
    Ok(())
}

/// Reads an optional nullable string field: absent, null (clear) or a string.
fn nullable_string(value: Option<&Value>) -> ApiResult<Option<Option<String>>> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err(bad("expected a string or null")),
    }
}

async fn publish_channel(state: &AppState, guild_id: &str, event: &'static str, channel: &Channel) {
    let targets = chat::member_ids(state, guild_id).await.unwrap_or_default();
    state.publish(
        targets,
        event,
        serde_json::to_value(channel).unwrap_or_default(),
    );
}

/// `PATCH /channels/{id}`: name, topic, parent category and position. Owner only.
pub async fn patch_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        let guild_id = channel.guild_id.clone().ok_or_else(chat::no_access)?;
        owned_guild(&state, &guild_id, &user.id).await?;
        let fields = body
            .as_object()
            .ok_or_else(|| bad("body must be an object"))?;

        let name = match fields.get("name") {
            None => channel.name.clone(),
            Some(Value::String(s)) => Some(chat::check_name(s)?),
            Some(_) => return Err(bad("name must be a string")),
        };
        let topic = match nullable_string(fields.get("topic"))? {
            None => channel.topic.clone(),
            Some(value) => value.filter(|t| !t.is_empty()),
        };
        let parent_id = match nullable_string(fields.get("parent_id"))? {
            None => channel.parent_id.clone(),
            Some(None) => None,
            Some(Some(parent)) => {
                if channel.kind == channel_type::GUILD_CATEGORY as i64 {
                    return Err(bad("a category cannot have a parent"));
                }
                check_parent(&state, &guild_id, &parent).await?;
                Some(parent)
            }
        };
        let position = match fields.get("position") {
            None => channel.position,
            Some(value) => value
                .as_i64()
                .ok_or_else(|| bad("position must be an integer"))?
                .max(0),
        };

        sqlx::query(
            "UPDATE channels SET name = ?, topic = ?, parent_id = ?, position = ? WHERE id = ?",
        )
        .bind(&name)
        .bind(&topic)
        .bind(&parent_id)
        .bind(position)
        .bind(&channel_id)
        .execute(&state.db)
        .await
        .map_err(chat::db)?;
        let channel =
            chat::channel_json(&state, chat::channel_row(&state, &channel_id).await?).await?;
        publish_channel(&state, &guild_id, "CHANNEL_UPDATE", &channel).await;
        Ok(ok_json(&channel))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /channels/{id}`: removes the channel with its messages and files. Owner only.
pub async fn delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        let guild_id = channel.guild_id.clone().ok_or_else(chat::no_access)?;
        owned_guild(&state, &guild_id, &user.id).await?;
        let json = chat::channel_json(&state, channel).await?;
        uploads::remove_for_channels(&state, std::slice::from_ref(&channel_id))
            .await
            .map_err(chat::db)?;
        // Children of a deleted category stay in the guild, uncategorized (ON DELETE SET NULL).
        sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(&channel_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        publish_channel(&state, &guild_id, "CHANNEL_DELETE", &json).await;
        Ok(ok_json(&json))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PATCH /guilds/{id}/channels`: bulk position and category changes. Owner only; 204 on success.
pub async fn reorder_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(entries): Json<Vec<Value>>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        owned_guild(&state, &guild_id, &user.id).await?;
        if entries.len() > 500 {
            return Err(bad("too many channels in one request"));
        }
        let mut changed = Vec::new();
        let mut tx = state.db.begin().await.map_err(chat::db)?;
        for entry in &entries {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("each entry needs an id"))?;
            let row = sqlx::query("SELECT guild_id, kind FROM channels WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(chat::db)?
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10003, "Unknown Channel"))?;
            let row_guild: Option<String> = row.get("guild_id");
            if row_guild.as_deref() != Some(guild_id.as_str()) {
                return Err(bad("channel is not in this guild"));
            }
            let kind: i64 = row.get("kind");
            if let Some(position) = entry.get("position").and_then(Value::as_i64) {
                sqlx::query("UPDATE channels SET position = ? WHERE id = ?")
                    .bind(position.max(0))
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(chat::db)?;
            }
            match nullable_string(entry.get("parent_id"))? {
                None => {}
                Some(None) => {
                    sqlx::query("UPDATE channels SET parent_id = NULL WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(chat::db)?;
                }
                Some(Some(parent)) => {
                    if kind == channel_type::GUILD_CATEGORY as i64 {
                        return Err(bad("a category cannot have a parent"));
                    }
                    let parent_row =
                        sqlx::query("SELECT guild_id, kind FROM channels WHERE id = ?")
                            .bind(&parent)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(chat::db)?
                            .ok_or_else(|| bad("parent must be a category in this guild"))?;
                    let parent_guild: Option<String> = parent_row.get("guild_id");
                    let parent_kind: i64 = parent_row.get("kind");
                    if parent_guild.as_deref() != Some(guild_id.as_str())
                        || parent_kind != channel_type::GUILD_CATEGORY as i64
                    {
                        return Err(bad("parent must be a category in this guild"));
                    }
                    sqlx::query("UPDATE channels SET parent_id = ? WHERE id = ?")
                        .bind(&parent)
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(chat::db)?;
                }
            }
            changed.push(id.to_string());
        }
        tx.commit().await.map_err(chat::db)?;
        for id in &changed {
            let row = chat::channel_row(&state, id).await?;
            let channel = chat::channel_json(&state, row).await?;
            publish_channel(&state, &guild_id, "CHANNEL_UPDATE", &channel).await;
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PATCH /guilds/{id}`: name. Owner only.
pub async fn patch_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let guild = owned_guild(&state, &guild_id, &user.id).await?;
        let name = match body.get("name") {
            None => guild.name.clone(),
            Some(Value::String(s)) => chat::check_name(s)?,
            Some(_) => return Err(bad("name must be a string")),
        };
        sqlx::query("UPDATE guilds SET name = ? WHERE id = ?")
            .bind(&name)
            .bind(&guild_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        let json =
            chat::guild_json(&state, chat::guild_row(&state, &guild_id).await?, true).await?;
        let targets = chat::member_ids(&state, &guild_id)
            .await
            .map_err(chat::db)?;
        state.publish(
            targets,
            "GUILD_UPDATE",
            serde_json::to_value(&json).unwrap_or_default(),
        );
        Ok(ok_json(&json))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /guilds/{id}`: removes the guild, its channels, messages and files. Owner only; 204.
pub async fn delete_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        owned_guild(&state, &guild_id, &user.id).await?;
        let targets = chat::member_ids(&state, &guild_id)
            .await
            .map_err(chat::db)?;
        let channel_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM channels WHERE guild_id = ?")
                .bind(&guild_id)
                .fetch_all(&state.db)
                .await
                .map_err(chat::db)?;
        uploads::remove_for_channels(&state, &channel_ids)
            .await
            .map_err(chat::db)?;
        sqlx::query("DELETE FROM guilds WHERE id = ?")
            .bind(&guild_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        state.publish(
            targets,
            "GUILD_DELETE",
            serde_json::json!({ "id": guild_id, "unavailable": false }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `GET /guilds/{id}/invites`: invites for the guild. Owner only.
pub async fn list_guild_invites(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let guild = owned_guild(&state, &guild_id, &user.id).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT code, channel_id FROM invites WHERE guild_id = ? ORDER BY created_at DESC",
        )
        .bind(&guild_id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?;
        let guild_json = chat::guild_json(&state, guild, false).await?;
        let mut invites = Vec::with_capacity(rows.len());
        for (code, channel_id) in rows {
            let channel =
                chat::channel_json(&state, chat::channel_row(&state, &channel_id).await?).await?;
            invites.push(Invite {
                code,
                guild: guild_json.clone(),
                channel,
            });
        }
        Ok(ok_json(&invites))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /invites/{code}`: the inviter or the guild owner can revoke an invite.
pub async fn delete_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let row =
            sqlx::query("SELECT guild_id, channel_id, inviter_id FROM invites WHERE code = ?")
                .bind(&code)
                .fetch_optional(&state.db)
                .await
                .map_err(chat::db)?
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10006, "Unknown Invite"))?;
        let guild_id: String = row.get("guild_id");
        let channel_id: String = row.get("channel_id");
        let inviter_id: String = row.get("inviter_id");
        let guild = chat::guild_row(&state, &guild_id).await?;
        if inviter_id != user.id && guild.owner_id != user.id {
            return Err(chat::no_access());
        }
        let invite = Invite {
            code: code.clone(),
            guild: chat::guild_json(&state, guild, false).await?,
            channel: chat::channel_json(&state, chat::channel_row(&state, &channel_id).await?)
                .await?,
        };
        sqlx::query("DELETE FROM invites WHERE code = ?")
            .bind(&code)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        Ok(ok_json(&invite))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

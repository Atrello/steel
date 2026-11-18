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
    perms::{self, MANAGE_CHANNELS, MANAGE_GUILD, MANAGE_ROLES},
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

/// Overwrites of one channel as clients see them.
pub async fn overwrites_for(state: &AppState, channel_id: &str) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
        &crate::db::sql("SELECT target_id, target_type, allow, deny FROM overwrites WHERE channel_id = ? ORDER BY target_type, CAST(target_id AS BIGINT)"),
    )
    .bind(channel_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, allow, deny)| serde_json::json!({ "id": id, "type": kind, "allow": allow, "deny": deny }))
        .collect())
}

/// Replaces a channel's overwrites with `entries`. Each entry names a role (type 0, `@everyone`
/// is the guild id) or a member (type 1) of this guild, with allow and deny bitfields.
pub async fn write_overwrites(
    state: &AppState,
    guild_id: &str,
    channel_id: &str,
    entries: &[Value],
) -> ApiResult<()> {
    if entries.len() > 100 {
        return Err(bad("too many permission overwrites"));
    }
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("each overwrite needs an id"))?;
        let kind = entry
            .get("type")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("each overwrite needs a type"))?;
        let bits = |field: &str| -> ApiResult<u64> {
            match entry.get(field) {
                None => Ok(0),
                Some(Value::String(s)) => s
                    .parse::<u64>()
                    .map_err(|_| bad("allow and deny must be numbers")),
                Some(Value::Number(n)) => n
                    .as_u64()
                    .ok_or_else(|| bad("allow and deny must be numbers")),
                Some(_) => Err(bad("allow and deny must be numbers")),
            }
        };
        let (allow, deny) = (bits("allow")?, bits("deny")?);
        match kind {
            0 => {
                if id != guild_id {
                    let found: Option<(String,)> = sqlx::query_as(&crate::db::sql(
                        "SELECT id FROM roles WHERE id = ? AND guild_id = ?",
                    ))
                    .bind(id)
                    .bind(guild_id)
                    .fetch_optional(&state.db)
                    .await
                    .map_err(chat::db)?;
                    if found.is_none() {
                        return Err(bad("overwrite role is not in this guild"));
                    }
                }
            }
            1 => {
                chat::require_member(state, guild_id, id)
                    .await
                    .map_err(|_| bad("overwrite member is not in this guild"))?;
            }
            _ => return Err(bad("overwrite type must be 0 or 1")),
        }
        parsed.push((id.to_string(), kind as i64, allow, deny));
    }
    let mut tx = state.db.begin().await.map_err(chat::db)?;
    sqlx::query(&crate::db::sql(
        "DELETE FROM overwrites WHERE channel_id = ?",
    ))
    .bind(channel_id)
    .execute(&mut *tx)
    .await
    .map_err(chat::db)?;
    for (id, kind, allow, deny) in parsed {
        sqlx::query(&crate::db::sql("INSERT OR REPLACE INTO overwrites (channel_id, target_id, target_type, allow, deny) VALUES (?, ?, ?, ?, ?)"))
            .bind(channel_id)
            .bind(id)
            .bind(kind)
            .bind(allow.to_string())
            .bind(deny.to_string())
            .execute(&mut *tx)
            .await
            .map_err(chat::db)?;
    }
    tx.commit().await.map_err(chat::db)?;
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
        perms::require_channel(&state, &guild_id, &channel_id, &user.id, MANAGE_CHANNELS).await?;
        let fields = body
            .as_object()
            .ok_or_else(|| bad("body must be an object"))?;
        if let Some(overwrites) = fields.get("permission_overwrites") {
            perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
            let list = overwrites
                .as_array()
                .ok_or_else(|| bad("permission_overwrites must be a list"))?;
            write_overwrites(&state, &guild_id, &channel_id, list).await?;
        }

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

        sqlx::query(&crate::db::sql(
            "UPDATE channels SET name = ?, topic = ?, parent_id = ?, position = ? WHERE id = ?",
        ))
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
        perms::require_channel(&state, &guild_id, &channel_id, &user.id, MANAGE_CHANNELS).await?;
        let json = chat::channel_json(&state, channel).await?;
        uploads::remove_for_channels(&state, std::slice::from_ref(&channel_id))
            .await
            .map_err(chat::db)?;
        // Children of a deleted category stay in the guild, uncategorized (ON DELETE SET NULL).
        sqlx::query(&crate::db::sql("DELETE FROM channels WHERE id = ?"))
            .bind(&channel_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        crate::audit::record(
            &state,
            &guild_id,
            &user.id,
            crate::audit::CHANNEL_DELETE,
            Some(&channel_id),
            serde_json::json!([{"key": "name", "old_value": json.name}]),
            None,
        )
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
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_CHANNELS).await?;
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
            let row = sqlx::query(&crate::db::sql(
                "SELECT guild_id, kind FROM channels WHERE id = ?",
            ))
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
                sqlx::query(&crate::db::sql(
                    "UPDATE channels SET position = ? WHERE id = ?",
                ))
                .bind(position.max(0))
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(chat::db)?;
            }
            match nullable_string(entry.get("parent_id"))? {
                None => {}
                Some(None) => {
                    sqlx::query(&crate::db::sql(
                        "UPDATE channels SET parent_id = NULL WHERE id = ?",
                    ))
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(chat::db)?;
                }
                Some(Some(parent)) => {
                    if kind == channel_type::GUILD_CATEGORY as i64 {
                        return Err(bad("a category cannot have a parent"));
                    }
                    let parent_row = sqlx::query(&crate::db::sql(
                        "SELECT guild_id, kind FROM channels WHERE id = ?",
                    ))
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
                    sqlx::query(&crate::db::sql(
                        "UPDATE channels SET parent_id = ? WHERE id = ?",
                    ))
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
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_GUILD).await?;
        let guild = chat::guild_row(&state, &guild_id).await?;
        let name = match body.get("name") {
            None => guild.name.clone(),
            Some(Value::String(s)) => chat::check_name(s)?,
            Some(_) => return Err(bad("name must be a string")),
        };
        sqlx::query(&crate::db::sql("UPDATE guilds SET name = ? WHERE id = ?"))
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
        let channel_ids: Vec<String> = sqlx::query_scalar(&crate::db::sql(
            "SELECT id FROM channels WHERE guild_id = ?",
        ))
        .bind(&guild_id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?;
        uploads::remove_for_channels(&state, &channel_ids)
            .await
            .map_err(chat::db)?;
        sqlx::query(&crate::db::sql("DELETE FROM guilds WHERE id = ?"))
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
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_GUILD).await?;
        let guild = chat::guild_row(&state, &guild_id).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(&crate::db::sql(
            "SELECT code, channel_id FROM invites WHERE guild_id = ? ORDER BY created_at DESC",
        ))
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
        let row = sqlx::query(&crate::db::sql(
            "SELECT guild_id, channel_id, inviter_id FROM invites WHERE code = ?",
        ))
        .bind(&code)
        .fetch_optional(&state.db)
        .await
        .map_err(chat::db)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10006, "Unknown Invite"))?;
        let guild_id: String = row.get("guild_id");
        let channel_id: String = row.get("channel_id");
        let inviter_id: String = row.get("inviter_id");
        let guild = chat::guild_row(&state, &guild_id).await?;
        if inviter_id != user.id {
            perms::require_guild(&state, &guild_id, &user.id, MANAGE_GUILD).await?;
        }
        let invite = Invite {
            code: code.clone(),
            guild: chat::guild_json(&state, guild, false).await?,
            channel: chat::channel_json(&state, chat::channel_row(&state, &channel_id).await?)
                .await?,
        };
        sqlx::query(&crate::db::sql("DELETE FROM invites WHERE code = ?"))
            .bind(&code)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        Ok(ok_json(&invite))
    }
    .await;
    result.unwrap_or_else(|e| e)
}

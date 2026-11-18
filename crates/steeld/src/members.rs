//! Membership and read-state routes: leave a group DM or a guild, guild member read and edit,
//! kick and ban, and read-state acks (per guild and bulk).
//! Phase 1 rules: the owner moderates; members can leave, and can edit their own nickname.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use flint_protocol::User;
use serde_json::{json, Value};

use crate::{
    api::{api_error, API_PREFIX},
    chat::{self, ApiResult},
    state::AppState,
};

const MAX_BULK_ACKS: usize = 100;
const MAX_NICK: usize = 32;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/ack"),
            post(ack_guild),
        )
        .route(
            &format!("{API_PREFIX}/read-states/ack-bulk"),
            post(ack_bulk),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/recipients/@me"),
            axum::routing::delete(leave_dm),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/guilds/{{guild_id}}"),
            axum::routing::delete(leave_guild),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/members/{{user_id}}"),
            get(get_member).patch(edit_member).delete(kick_member),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/bans/{{user_id}}"),
            put(ban_member).delete(unban_member),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

/// Move a member's read marker forward only, and tell that user.
async fn advance_read(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    message_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        &crate::db::sql("INSERT INTO read_states (user_id, channel_id, last_message_id) VALUES (?, ?, ?) \
         ON CONFLICT(user_id, channel_id) DO UPDATE SET last_message_id = excluded.last_message_id \
         WHERE CAST(excluded.last_message_id AS BIGINT) > CAST(COALESCE(read_states.last_message_id, '0') AS INTEGER)"),
    )
    .bind(user_id)
    .bind(channel_id)
    .bind(message_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// `POST /guilds/{id}/ack`: mark every channel in the guild as read up to its newest message.
async fn ack_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let channels: Vec<(String,)> = sqlx::query_as(&crate::db::sql("SELECT id FROM channels WHERE guild_id = ?"))
            .bind(&guild_id)
            .fetch_all(&state.db)
            .await
            .map_err(db)?;
        for (channel_id,) in channels {
            let newest: Option<(String,)> = sqlx::query_as(
                &crate::db::sql("SELECT id FROM messages WHERE channel_id = ? ORDER BY CAST(id AS BIGINT) DESC LIMIT 1"),
            )
            .bind(&channel_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
            if let Some((message_id,)) = newest {
                advance_read(&state, &user.id, &channel_id, &message_id).await.map_err(db)?;
                state.publish(
                    vec![user.id.clone()],
                    "MESSAGE_ACK",
                    json!({ "channel_id": channel_id, "message_id": message_id, "mention_count": 0 }),
                );
            }
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `POST /read-states/ack-bulk`: `{ "read_states": [{ "channel_id", "message_id" }] }`.
async fn ack_bulk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let items = body
            .get("read_states")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    50035,
                    "read_states must be an array",
                )
            })?;
        if items.len() > MAX_BULK_ACKS {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                50035,
                format!("at most {MAX_BULK_ACKS} read states per request"),
            ));
        }
        for item in items {
            let (Some(channel_id), Some(message_id)) = (
                item.get("channel_id").and_then(Value::as_str),
                item.get("message_id").and_then(Value::as_str),
            ) else {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    50035,
                    "each read state needs channel_id and message_id",
                ));
            };
            let channel = chat::channel_row(&state, channel_id).await?;
            chat::require_channel(&state, &channel, &user.id).await?;
            advance_read(&state, &user.id, channel_id, message_id)
                .await
                .map_err(db)?;
            state.publish(
                vec![user.id.clone()],
                "MESSAGE_ACK",
                json!({ "channel_id": channel_id, "message_id": message_id, "mention_count": 0 }),
            );
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /channels/{id}/recipients/@me`: leave a DM or group DM. The channel is removed when nobody is left.
async fn leave_dm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        if channel.guild_id.is_some() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                50035,
                "only DMs can be left",
            ));
        }
        let removed = sqlx::query(&crate::db::sql(
            "DELETE FROM channel_recipients WHERE channel_id = ? AND user_id = ?",
        ))
        .bind(&channel_id)
        .bind(&user.id)
        .execute(&state.db)
        .await
        .map_err(db)?
        .rows_affected();
        if removed == 0 {
            return Err(chat::missing_channel());
        }
        // The leaver stops seeing the channel either way.
        state.publish(
            vec![user.id.clone()],
            "CHANNEL_DELETE",
            json!({ "id": channel_id, "type": channel.kind }),
        );

        let remaining = chat::channel_audience(&state, &channel).await.map_err(db)?;
        if remaining.is_empty() {
            sqlx::query(&crate::db::sql("DELETE FROM channels WHERE id = ?"))
                .bind(&channel_id)
                .execute(&state.db)
                .await
                .map_err(db)?;
        } else {
            state.publish(
                remaining,
                "CHANNEL_RECIPIENT_REMOVE",
                json!({ "channel_id": channel_id, "user": { "id": user.id } }),
            );
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /users/@me/guilds/{id}`: leave a guild. The owner must delete it instead.
async fn leave_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let guild = chat::guild_row(&state, &guild_id).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        if guild.owner_id == user.id {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                50055,
                "the owner cannot leave their own guild",
            ));
        }
        sqlx::query(&crate::db::sql(
            "DELETE FROM guild_members WHERE guild_id = ? AND user_id = ?",
        ))
        .bind(&guild_id)
        .bind(&user.id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        state.publish(
            vec![user.id.clone()],
            "GUILD_DELETE",
            json!({ "id": guild_id }),
        );
        let remaining = chat::member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(
            remaining,
            "GUILD_MEMBER_REMOVE",
            json!({ "guild_id": guild_id, "user": { "id": user.id } }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Member object as clients expect it. Roles arrive with Phase 2.
fn member_json(id: &str, username: &str, nick: Option<String>, joined_at: &str) -> Value {
    json!({
        "user": chat::plain_user(id.to_string(), username.to_string()),
        "nick": nick,
        "roles": [],
        "joined_at": joined_at,
        "deaf": false,
        "mute": false,
        "communication_disabled_until": null,
    })
}

async fn member_row(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<Value> {
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        &crate::db::sql("SELECT u.id, u.username, m.joined_at, m.nick FROM guild_members m JOIN users u ON u.id = m.user_id \
         WHERE m.guild_id = ? AND m.user_id = ?"),
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?;
    let (id, username, joined_at, nick) = row.ok_or_else(chat::missing_member)?;
    let roles = crate::roles::member_role_ids(state, guild_id, user_id).await?;
    let mut member = member_json(&id, &username, nick, &joined_at);
    member["roles"] = json!(roles);
    Ok(member)
}

/// `GET /guilds/{id}/members/{user}`: any member can read another member.
async fn get_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &me.id).await?;
        Ok(Json(member_row(&state, &guild_id, &user_id).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PATCH /guilds/{id}/members/{user}`: nickname only. Members can change their own; the owner can change anyone's.
async fn edit_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &me.id).await?;
        let fields = body.as_object().ok_or_else(|| api_error(StatusCode::BAD_REQUEST, 50035, "body must be an object"))?;
        if let Some(unsupported) = fields.keys().find(|k| k.as_str() != "nick") {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                50035,
                format!("{unsupported} is not supported yet"),
            ));
        }
        let needed = if me.id == user_id {
            crate::perms::CHANGE_NICKNAME
        } else {
            crate::perms::MANAGE_NICKNAMES
        };
        crate::perms::require_guild(&state, &guild_id, &me.id, needed).await?;
        crate::perms::require_above_member(&state, &guild_id, &me.id, &user_id).await?;
        let nick = match fields.get("nick") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.chars().count() > MAX_NICK {
                    return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("nick must be at most {MAX_NICK} characters")));
                }
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            Some(_) => return Err(api_error(StatusCode::BAD_REQUEST, 50035, "nick must be a string or null")),
        };
        let updated = sqlx::query(&crate::db::sql("UPDATE guild_members SET nick = ? WHERE guild_id = ? AND user_id = ?"))
            .bind(&nick)
            .bind(&guild_id)
            .bind(&user_id)
            .execute(&state.db)
            .await
            .map_err(db)?
            .rows_affected();
        if updated == 0 {
            return Err(chat::missing_member());
        }
        let member = member_row(&state, &guild_id, &user_id).await?;
        let targets = chat::member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(
            targets,
            "GUILD_MEMBER_UPDATE",
            json!({ "guild_id": guild_id, "user": member["user"], "nick": member["nick"], "roles": [] }),
        );
        Ok(Json(member).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Remove a member's membership and tell everyone. Returns the removed user's ID.
async fn remove_membership(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<()> {
    sqlx::query(&crate::db::sql(
        "DELETE FROM guild_members WHERE guild_id = ? AND user_id = ?",
    ))
    .bind(guild_id)
    .bind(user_id)
    .execute(&state.db)
    .await
    .map_err(db)?;
    state.publish(
        vec![user_id.to_string()],
        "GUILD_DELETE",
        json!({ "id": guild_id }),
    );
    let remaining = chat::member_ids(state, guild_id).await.map_err(db)?;
    state.publish(
        remaining,
        "GUILD_MEMBER_REMOVE",
        json!({ "guild_id": guild_id, "user": { "id": user_id } }),
    );
    Ok(())
}

/// Shared checks for kick and ban: the caller owns the guild, and the target is neither the owner nor the caller.
async fn moderation_target(
    state: &AppState,
    headers: &HeaderMap,
    guild_id: &str,
    user_id: &str,
    bit: u64,
) -> ApiResult<User> {
    let me = chat::authed(state, headers).await?;
    crate::perms::require_guild(state, guild_id, &me.id, bit).await?;
    crate::perms::require_above_member(state, guild_id, &me.id, user_id).await?;
    let guild = chat::guild_row(state, guild_id).await?;
    if user_id == guild.owner_id || user_id == me.id {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "that user cannot be moderated",
        ));
    }
    Ok(me)
}

/// `DELETE /guilds/{id}/members/{user}`: kick (owner only). The user may rejoin with an invite.
async fn kick_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        moderation_target(
            &state,
            &headers,
            &guild_id,
            &user_id,
            crate::perms::KICK_MEMBERS,
        )
        .await?;
        let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?",
        ))
        .bind(&guild_id)
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if found.is_none() {
            return Err(chat::missing_member());
        }
        remove_membership(&state, &guild_id, &user_id).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PUT /guilds/{id}/bans/{user}`: ban (owner only). Removes membership; invites no longer work for them.
async fn ban_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        moderation_target(
            &state,
            &headers,
            &guild_id,
            &user_id,
            crate::perms::BAN_MEMBERS,
        )
        .await?;
        sqlx::query(&crate::db::sql(
            "INSERT OR REPLACE INTO guild_bans (guild_id, user_id, created_at) VALUES (?, ?, ?)",
        ))
        .bind(&guild_id)
        .bind(&user_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.db)
        .await
        .map_err(db)?;
        let was_member: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?",
        ))
        .bind(&guild_id)
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if was_member.is_some() {
            remove_membership(&state, &guild_id, &user_id).await?;
        }
        let remaining = chat::member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(
            remaining,
            "GUILD_BAN_ADD",
            json!({ "guild_id": guild_id, "user": { "id": user_id } }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE /guilds/{id}/bans/{user}`: lift a ban (owner only).
async fn unban_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = chat::authed(&state, &headers).await?;
        crate::perms::require_guild(&state, &guild_id, &me.id, crate::perms::BAN_MEMBERS).await?;
        sqlx::query(&crate::db::sql(
            "DELETE FROM guild_bans WHERE guild_id = ? AND user_id = ?",
        ))
        .bind(&guild_id)
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        let remaining = chat::member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(
            remaining,
            "GUILD_BAN_REMOVE",
            json!({ "guild_id": guild_id, "user": { "id": user_id } }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

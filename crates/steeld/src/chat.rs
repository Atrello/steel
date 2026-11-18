//! Guilds ("servers"), channels, messages, invites and typing.
//! Phase 1 permissions are membership-only: members can read and post in their guild's channels,
//! the owner can create channels and delete any message, and authors can edit or delete their own.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use flint_protocol::{
    channel_type, Channel, CreateChannel, CreateGuild, CreateMessage, EditMessage, Guild, Invite, Message,
    MessageReference, User,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::{api_error, user_from_headers, API_PREFIX},
    auth,
    state::AppState,
};

const MAX_CONTENT: usize = 2000;
const MAX_NAME: usize = 100;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 100;

type ApiResult<T> = Result<T, Response>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(&format!("{API_PREFIX}/guilds"), post(create_guild))
        .route(&format!("{API_PREFIX}/users/@me/guilds"), get(my_guilds))
        .route(&format!("{API_PREFIX}/guilds/{{guild_id}}"), get(get_guild))
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/channels"),
            get(list_channels).post(create_channel),
        )
        .route(&format!("{API_PREFIX}/channels/{{channel_id}}"), get(get_channel))
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages"),
            get(list_messages).post(send_message),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}"),
            axum::routing::patch(edit_message).delete(delete_message),
        )
        .route(&format!("{API_PREFIX}/channels/{{channel_id}}/typing"), post(typing))
        .route(&format!("{API_PREFIX}/channels/{{channel_id}}/invites"), post(create_invite))
        .route(&format!("{API_PREFIX}/invites/{{code}}"), get(get_invite).post(accept_invite))
}

// ---------------------------------------------------------------------------
// Rows and helpers
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct GuildRow {
    id: String,
    name: String,
    owner_id: String,
}

#[derive(sqlx::FromRow)]
struct ChannelRow {
    id: String,
    guild_id: String,
    kind: i64,
    name: String,
    topic: Option<String>,
    position: i64,
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    channel_id: String,
    author_id: String,
    username: String,
    content: String,
    created_at: String,
    edited_at: Option<String>,
    reply_to_id: Option<String>,
    guild_id: String,
}

fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!("database error: {err}");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, 0, "internal server error")
}

fn db(err: sqlx::Error) -> Response {
    internal(err)
}

fn missing(what: &str) -> Response {
    api_error(StatusCode::NOT_FOUND, 10003, format!("Unknown {what}"))
}

fn no_access() -> Response {
    api_error(StatusCode::FORBIDDEN, 50001, "Missing Access")
}

async fn authed(state: &AppState, headers: &HeaderMap) -> ApiResult<User> {
    user_from_headers(state, headers)
        .await
        .map_err(db)?
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"))
}

fn check_name(name: &str) -> ApiResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME {
        return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("name must be 1-{MAX_NAME} characters")));
    }
    Ok(trimmed.to_string())
}

async fn require_member(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<()> {
    let found: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
    found.map(|_| ()).ok_or_else(no_access)
}

async fn guild_row(state: &AppState, guild_id: &str) -> ApiResult<GuildRow> {
    sqlx::query_as("SELECT id, name, owner_id FROM guilds WHERE id = ?")
        .bind(guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("guild"))
}

async fn channel_row(state: &AppState, channel_id: &str) -> ApiResult<ChannelRow> {
    sqlx::query_as("SELECT id, guild_id, kind, name, topic, position FROM channels WHERE id = ?")
        .bind(channel_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("channel"))
}

fn channel_json(row: ChannelRow) -> Channel {
    Channel {
        id: row.id,
        kind: row.kind as u8,
        guild_id: Some(row.guild_id),
        name: Some(row.name),
        position: row.position,
        topic: row.topic,
    }
}

async fn guild_channels(state: &AppState, guild_id: &str) -> Result<Vec<Channel>, sqlx::Error> {
    let rows: Vec<ChannelRow> = sqlx::query_as(
        "SELECT id, guild_id, kind, name, topic, position FROM channels WHERE guild_id = ? ORDER BY position, CAST(id AS INTEGER)",
    )
    .bind(guild_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(channel_json).collect())
}

async fn guild_json(state: &AppState, row: GuildRow, with_channels: bool) -> ApiResult<Guild> {
    let channels = if with_channels { guild_channels(state, &row.id).await.map_err(db)? } else { Vec::new() };
    Ok(Guild { id: row.id, name: row.name, owner_id: row.owner_id, channels })
}

async fn member_ids(state: &AppState, guild_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT user_id FROM guild_members WHERE guild_id = ?")
        .bind(guild_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Guilds the user belongs to, with their channels. Used by READY.
pub async fn guilds_for_user(state: &AppState, user_id: &str) -> Result<Vec<Guild>, sqlx::Error> {
    let rows: Vec<GuildRow> = sqlx::query_as(
        "SELECT g.id, g.name, g.owner_id FROM guilds g JOIN guild_members m ON m.guild_id = g.id \
         WHERE m.user_id = ? ORDER BY CAST(g.id AS INTEGER)",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let mut guilds = Vec::with_capacity(rows.len());
    for row in rows {
        let channels = guild_channels(state, &row.id).await?;
        guilds.push(Guild { id: row.id, name: row.name, owner_id: row.owner_id, channels });
    }
    Ok(guilds)
}

fn message_json(row: MessageRow) -> Message {
    Message {
        id: row.id,
        channel_id: row.channel_id.clone(),
        author: User { id: row.author_id, username: row.username, global_name: None, avatar: None, bot: false },
        content: row.content,
        timestamp: row.created_at,
        edited_timestamp: row.edited_at,
        message_reference: row.reply_to_id.map(|reply| MessageReference {
            message_id: Some(reply),
            channel_id: Some(row.channel_id),
            guild_id: Some(row.guild_id),
        }),
        kind: 0,
    }
}

const MESSAGE_SELECT: &str = "SELECT m.id, m.channel_id, m.author_id, u.username, m.content, m.created_at, m.edited_at, \
     m.reply_to_id, c.guild_id FROM messages m JOIN users u ON u.id = m.author_id JOIN channels c ON c.id = m.channel_id";

async fn message_by_id(state: &AppState, message_id: &str) -> ApiResult<MessageRow> {
    sqlx::query_as(&format!("{MESSAGE_SELECT} WHERE m.id = ?"))
        .bind(message_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("message"))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn random_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let token = auth::new_token();
    token.bytes().take(8).map(|b| ALPHABET[(b as usize) % ALPHABET.len()] as char).collect()
}

// ---------------------------------------------------------------------------
// Guilds
// ---------------------------------------------------------------------------

async fn create_guild(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<CreateGuild>) -> Response {
    match create_guild_inner(&state, &headers, req).await {
        Ok(v) => v,
        Err(e) => e,
    }
}

async fn create_guild_inner(state: &AppState, headers: &HeaderMap, req: CreateGuild) -> ApiResult<Response> {
    let user = authed(state, headers).await?;
    let name = check_name(&req.name)?;
    let guild_id = auth::new_id();
    let general_id = auth::new_id();
    let now = now_iso();

    let mut tx = state.db.begin().await.map_err(db)?;
    sqlx::query("INSERT INTO guilds (id, name, owner_id, created_at) VALUES (?, ?, ?, ?)")
        .bind(&guild_id).bind(&name).bind(&user.id).bind(&now)
        .execute(&mut *tx).await.map_err(db)?;
    sqlx::query("INSERT INTO guild_members (guild_id, user_id, joined_at) VALUES (?, ?, ?)")
        .bind(&guild_id).bind(&user.id).bind(&now)
        .execute(&mut *tx).await.map_err(db)?;
    sqlx::query("INSERT INTO channels (id, guild_id, kind, name, position, created_at) VALUES (?, ?, ?, 'general', 0, ?)")
        .bind(&general_id).bind(&guild_id).bind(channel_type::GUILD_TEXT as i64).bind(&now)
        .execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;

    let guild = guild_json(state, GuildRow { id: guild_id, name, owner_id: user.id.clone() }, true).await?;
    state.publish(vec![user.id.clone()], "GUILD_CREATE", serde_json::to_value(&guild).unwrap_or_default());
    Ok((StatusCode::CREATED, Json(guild)).into_response())
}

async fn my_guilds(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let guilds = guilds_for_user(&state, &user.id).await.map_err(db)?;
        Ok(Json(guilds).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn get_guild(State(state): State<AppState>, headers: HeaderMap, Path(guild_id): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        require_member(&state, &guild_id, &user.id).await?;
        let row = guild_row(&state, &guild_id).await?;
        Ok(Json(guild_json(&state, row, true).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

async fn list_channels(State(state): State<AppState>, headers: HeaderMap, Path(guild_id): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        require_member(&state, &guild_id, &user.id).await?;
        Ok(Json(guild_channels(&state, &guild_id).await.map_err(db)?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(req): Json<CreateChannel>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let guild = guild_row(&state, &guild_id).await?;
        if guild.owner_id != user.id {
            return Err(no_access());
        }
        let name = check_name(&req.name)?;
        let kind = req.kind.unwrap_or(channel_type::GUILD_TEXT);
        if kind != channel_type::GUILD_TEXT {
            return Err(api_error(StatusCode::BAD_REQUEST, 50024, "only text channels are supported"));
        }
        let channel_id = auth::new_id();
        let position: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM channels WHERE guild_id = ?")
            .bind(&guild_id)
            .fetch_one(&state.db)
            .await
            .map_err(db)?;
        sqlx::query("INSERT INTO channels (id, guild_id, kind, name, topic, position, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&channel_id).bind(&guild_id).bind(kind as i64).bind(&name)
            .bind(req.topic.as_deref().filter(|t| !t.is_empty())).bind(position).bind(now_iso())
            .execute(&state.db).await.map_err(db)?;
        let channel = channel_json(channel_row(&state, &channel_id).await?);
        let targets = member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(targets, "CHANNEL_CREATE", serde_json::to_value(&channel).unwrap_or_default());
        Ok((StatusCode::CREATED, Json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn get_channel(State(state): State<AppState>, headers: HeaderMap, Path(channel_id): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_member(&state, &channel.guild_id, &user.id).await?;
        Ok(Json(channel_json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<u32>,
    pub before: Option<String>,
}

async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_member(&state, &channel.guild_id, &user.id).await?;
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let rows: Vec<MessageRow> = match q.before {
            Some(before) => sqlx::query_as(&format!(
                "{MESSAGE_SELECT} WHERE m.channel_id = ? AND CAST(m.id AS INTEGER) < CAST(? AS INTEGER) \
                 ORDER BY CAST(m.id AS INTEGER) DESC LIMIT ?"
            ))
            .bind(&channel_id).bind(before).bind(limit)
            .fetch_all(&state.db).await.map_err(db)?,
            None => sqlx::query_as(&format!(
                "{MESSAGE_SELECT} WHERE m.channel_id = ? ORDER BY CAST(m.id AS INTEGER) DESC LIMIT ?"
            ))
            .bind(&channel_id).bind(limit)
            .fetch_all(&state.db).await.map_err(db)?,
        };
        let messages: Vec<Message> = rows.into_iter().map(message_json).collect();
        Ok(Json(messages).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(req): Json<CreateMessage>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_member(&state, &channel.guild_id, &user.id).await?;
        let content = req.content.trim_end().to_string();
        if content.trim().is_empty() || content.chars().count() > MAX_CONTENT {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("content must be 1-{MAX_CONTENT} characters")));
        }
        let reply_to = match req.message_reference.as_ref().and_then(|r| r.message_id.clone()) {
            Some(reply_id) => {
                let target = message_by_id(&state, &reply_id).await?;
                if target.channel_id != channel_id {
                    return Err(api_error(StatusCode::BAD_REQUEST, 50035, "replies must target a message in this channel"));
                }
                Some(reply_id)
            }
            None => None,
        };
        let message_id = auth::new_id();
        let now = now_iso();
        sqlx::query("INSERT INTO messages (id, channel_id, author_id, content, reply_to_id, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&message_id).bind(&channel_id).bind(&user.id).bind(&content).bind(reply_to).bind(&now)
            .execute(&state.db).await.map_err(db)?;
        let message = message_json(message_by_id(&state, &message_id).await?);
        let targets = member_ids(&state, &channel.guild_id).await.map_err(db)?;
        state.publish(targets, "MESSAGE_CREATE", serde_json::to_value(&message).unwrap_or_default());
        Ok((StatusCode::OK, Json(message)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn edit_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
    Json(req): Json<EditMessage>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let existing = message_by_id(&state, &message_id).await?;
        if existing.channel_id != channel_id {
            return Err(missing("message"));
        }
        if existing.author_id != user.id {
            return Err(no_access());
        }
        let content = req.content.trim_end().to_string();
        if content.trim().is_empty() || content.chars().count() > MAX_CONTENT {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("content must be 1-{MAX_CONTENT} characters")));
        }
        sqlx::query("UPDATE messages SET content = ?, edited_at = ? WHERE id = ?")
            .bind(&content).bind(now_iso()).bind(&message_id)
            .execute(&state.db).await.map_err(db)?;
        let message = message_json(message_by_id(&state, &message_id).await?);
        let targets = member_ids(&state, &existing.guild_id).await.map_err(db)?;
        state.publish(targets, "MESSAGE_UPDATE", serde_json::to_value(&message).unwrap_or_default());
        Ok(Json(message).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn delete_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let existing = message_by_id(&state, &message_id).await?;
        if existing.channel_id != channel_id {
            return Err(missing("message"));
        }
        let guild = guild_row(&state, &existing.guild_id).await?;
        if existing.author_id != user.id && guild.owner_id != user.id {
            return Err(no_access());
        }
        sqlx::query("DELETE FROM messages WHERE id = ?").bind(&message_id).execute(&state.db).await.map_err(db)?;
        let targets = member_ids(&state, &existing.guild_id).await.map_err(db)?;
        state.publish(
            targets,
            "MESSAGE_DELETE",
            json!({ "id": message_id, "channel_id": channel_id, "guild_id": existing.guild_id }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn typing(State(state): State<AppState>, headers: HeaderMap, Path(channel_id): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_member(&state, &channel.guild_id, &user.id).await?;
        let targets = member_ids(&state, &channel.guild_id).await.map_err(db)?;
        state.publish(
            targets,
            "TYPING_START",
            json!({
                "channel_id": channel_id,
                "guild_id": channel.guild_id,
                "user_id": user.id,
                "timestamp": chrono::Utc::now().timestamp(),
            }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

// ---------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------

async fn create_invite(State(state): State<AppState>, headers: HeaderMap, Path(channel_id): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_member(&state, &channel.guild_id, &user.id).await?;
        let code = random_code();
        sqlx::query("INSERT INTO invites (code, guild_id, channel_id, inviter_id, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&code).bind(&channel.guild_id).bind(&channel_id).bind(&user.id).bind(now_iso())
            .execute(&state.db).await.map_err(db)?;
        let guild = guild_json(&state, guild_row(&state, &channel.guild_id).await?, false).await?;
        Ok((StatusCode::OK, Json(Invite { code, guild, channel: channel_json(channel) })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn invite_row(state: &AppState, code: &str) -> ApiResult<(String, String)> {
    sqlx::query_as("SELECT guild_id, channel_id FROM invites WHERE code = ?")
        .bind(code)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("invite"))
}

async fn get_invite(State(state): State<AppState>, Path(code): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let (guild_id, channel_id) = invite_row(&state, &code).await?;
        let guild = guild_json(&state, guild_row(&state, &guild_id).await?, false).await?;
        let channel = channel_json(channel_row(&state, &channel_id).await?);
        Ok(Json(Invite { code, guild, channel }).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn accept_invite(State(state): State<AppState>, headers: HeaderMap, Path(code): Path<String>) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let (guild_id, _channel_id) = invite_row(&state, &code).await?;
        sqlx::query("INSERT OR IGNORE INTO guild_members (guild_id, user_id, joined_at) VALUES (?, ?, ?)")
            .bind(&guild_id).bind(&user.id).bind(now_iso())
            .execute(&state.db).await.map_err(db)?;
        let guild = guild_json(&state, guild_row(&state, &guild_id).await?, true).await?;
        state.publish(vec![user.id.clone()], "GUILD_CREATE", serde_json::to_value(&guild).unwrap_or_default());
        let members = member_ids(&state, &guild_id).await.map_err(db)?;
        let member = json!({ "guild_id": guild_id, "user": user });
        let others: Vec<String> = members.into_iter().filter(|m| *m != user.id).collect();
        state.publish(others, "GUILD_MEMBER_ADD", member);
        Ok(Json(guild).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

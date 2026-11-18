//! Guilds ("servers"), channels, messages, invites and typing.
//! Phase 1 permissions are membership-only: members can read and post in their guild's channels,
//! the owner can create channels and delete any message, and authors can edit or delete their own.

use crate::uploads::{self, PendingFile, MAX_FILES};
use axum::{extract::FromRequest, http::header};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use flint_protocol::{
    channel_type, Channel, CreateChannel, CreateGuild, CreateMessage, EditMessage, Guild, Invite,
    Message, MessageReference, User,
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

pub type ApiResult<T> = Result<T, Response>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(&format!("{API_PREFIX}/guilds"), post(create_guild))
        .route(&format!("{API_PREFIX}/users/@me/guilds"), get(my_guilds))
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}"),
            get(get_guild)
                .patch(crate::channels::patch_guild)
                .delete(crate::channels::delete_guild),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/channels"),
            get(list_channels)
                .post(create_channel)
                .patch(crate::channels::reorder_channels),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/invites"),
            get(crate::channels::list_guild_invites),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}"),
            get(get_channel)
                .patch(crate::channels::patch_channel)
                .delete(crate::channels::delete_channel),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages"),
            get(list_messages).post(send_message),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}"),
            axum::routing::patch(edit_message).delete(delete_message),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/typing"),
            post(typing),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/invites"),
            post(create_invite),
        )
        .route(
            &format!("{API_PREFIX}/invites/{{code}}"),
            get(get_invite)
                .post(accept_invite)
                .delete(crate::channels::delete_invite),
        )
}

// ---------------------------------------------------------------------------
// Rows and helpers
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
pub struct GuildRow {
    pub id: String,
    pub name: String,
    pub owner_id: String,
}

#[derive(sqlx::FromRow)]
pub struct ChannelRow {
    pub id: String,
    pub guild_id: Option<String>,
    pub kind: i64,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub position: i64,
    pub parent_id: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct MessageRow {
    pub(crate) id: String,
    pub(crate) channel_id: String,
    pub(crate) author_id: String,
    pub(crate) username: String,
    pub(crate) content: String,
    pub(crate) created_at: String,
    pub(crate) edited_at: Option<String>,
    pub(crate) reply_to_id: Option<String>,
    pub(crate) guild_id: Option<String>,
}

fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!("database error: {err}");
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        0,
        "internal server error",
    )
}

pub fn db(err: sqlx::Error) -> Response {
    internal(err)
}

pub fn missing_channel() -> Response {
    missing("channel")
}

pub fn missing_message() -> Response {
    missing("message")
}

pub fn missing_member() -> Response {
    api_error(StatusCode::NOT_FOUND, 10007, "Unknown Member")
}

fn missing(what: &str) -> Response {
    api_error(StatusCode::NOT_FOUND, 10003, format!("Unknown {what}"))
}

pub fn no_access() -> Response {
    api_error(StatusCode::FORBIDDEN, 50001, "Missing Access")
}

pub async fn authed(state: &AppState, headers: &HeaderMap) -> ApiResult<User> {
    user_from_headers(state, headers)
        .await
        .map_err(db)?
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"))
}

pub fn check_name(name: &str) -> ApiResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            50035,
            format!("name must be 1-{MAX_NAME} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

/// Access to a channel: guild members for guild channels, recipients for DMs.
pub async fn require_channel(
    state: &AppState,
    channel: &ChannelRow,
    user_id: &str,
) -> ApiResult<()> {
    match &channel.guild_id {
        Some(guild_id) => require_member(state, guild_id, user_id).await,
        None => {
            let found: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM channel_recipients WHERE channel_id = ? AND user_id = ?",
            )
            .bind(&channel.id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
            found.map(|_| ()).ok_or_else(no_access)
        }
    }
}

/// Everyone who should receive events for a channel.
pub async fn channel_audience(
    state: &AppState,
    channel: &ChannelRow,
) -> Result<Vec<String>, sqlx::Error> {
    match &channel.guild_id {
        Some(guild_id) => member_ids(state, guild_id).await,
        None => {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT user_id FROM channel_recipients WHERE channel_id = ?")
                    .bind(&channel.id)
                    .fetch_all(&state.db)
                    .await?;
            Ok(rows.into_iter().map(|(id,)| id).collect())
        }
    }
}

pub async fn require_member(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<()> {
    let found: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
    found.map(|_| ()).ok_or_else(no_access)
}

pub async fn guild_row(state: &AppState, guild_id: &str) -> ApiResult<GuildRow> {
    sqlx::query_as("SELECT id, name, owner_id FROM guilds WHERE id = ?")
        .bind(guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("guild"))
}

pub async fn channel_row(state: &AppState, channel_id: &str) -> ApiResult<ChannelRow> {
    sqlx::query_as(
        "SELECT id, guild_id, kind, name, topic, position, parent_id FROM channels WHERE id = ?",
    )
    .bind(channel_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?
    .ok_or_else(|| missing("channel"))
}

/// Channel as sent to clients. DM channels include their recipients.
pub async fn channel_json(state: &AppState, row: ChannelRow) -> ApiResult<Channel> {
    let recipients = if row.guild_id.is_none() {
        let users: Vec<(String, String)> = sqlx::query_as(
            "SELECT u.id, u.username FROM channel_recipients r JOIN users u ON u.id = r.user_id WHERE r.channel_id = ? ORDER BY CAST(u.id AS INTEGER)",
        )
        .bind(&row.id)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        users
            .into_iter()
            .map(|(id, username)| plain_user(id, username))
            .collect()
    } else {
        Vec::new()
    };
    let permission_overwrites = crate::channels::overwrites_for(state, &row.id)
        .await
        .map_err(db)?;
    Ok(Channel {
        id: row.id,
        kind: row.kind as u8,
        guild_id: row.guild_id,
        name: row.name,
        position: row.position,
        topic: row.topic,
        parent_id: row.parent_id,
        permission_overwrites,
        recipients,
    })
}

pub fn plain_user(id: String, username: String) -> User {
    User {
        id,
        username,
        global_name: None,
        avatar: None,
        bot: false,
    }
}

async fn guild_channel_rows(
    state: &AppState,
    guild_id: &str,
) -> Result<Vec<ChannelRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, guild_id, kind, name, topic, position, parent_id FROM channels WHERE guild_id = ? ORDER BY position, CAST(id AS INTEGER)",
    )
    .bind(guild_id)
    .fetch_all(&state.db)
    .await
}

/// Guild channel without recipients (guild channels never have any).
fn guild_channel(row: ChannelRow) -> Channel {
    Channel {
        id: row.id,
        kind: row.kind as u8,
        guild_id: row.guild_id,
        name: row.name,
        position: row.position,
        topic: row.topic,
        parent_id: row.parent_id,
        permission_overwrites: Vec::new(),
        recipients: Vec::new(),
    }
}

pub async fn guild_channels(state: &AppState, guild_id: &str) -> ApiResult<Vec<Channel>> {
    let mut channels = Vec::new();
    for row in guild_channel_rows(state, guild_id).await.map_err(db)? {
        channels.push(
            guild_channel_with_overwrites(state, row)
                .await
                .map_err(db)?,
        );
    }
    Ok(channels)
}

/// Guild channel with its permission overwrites filled in.
async fn guild_channel_with_overwrites(
    state: &AppState,
    row: ChannelRow,
) -> Result<Channel, sqlx::Error> {
    let id = row.id.clone();
    let mut channel = guild_channel(row);
    channel.permission_overwrites = crate::channels::overwrites_for(state, &id).await?;
    Ok(channel)
}

pub async fn guild_json(state: &AppState, row: GuildRow, with_channels: bool) -> ApiResult<Guild> {
    let channels = if with_channels {
        guild_channels(state, &row.id).await?
    } else {
        Vec::new()
    };
    let roles = crate::roles::roles_for_guild(state, &row.id).await?;
    Ok(Guild {
        id: row.id,
        name: row.name,
        owner_id: row.owner_id,
        channels,
        roles,
        features: Vec::new(),
    })
}

pub async fn member_ids(state: &AppState, guild_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT user_id FROM guild_members WHERE guild_id = ?")
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
        let mut channels = Vec::new();
        for channel_row in guild_channel_rows(state, &row.id).await? {
            channels.push(guild_channel_with_overwrites(state, channel_row).await?);
        }
        let roles = crate::roles::roles_for_guild_db(state, &row.id).await?;
        guilds.push(Guild {
            id: row.id,
            name: row.name,
            owner_id: row.owner_id,
            channels,
            roles,
            features: Vec::new(),
        });
    }
    Ok(guilds)
}

pub fn message_json(row: MessageRow) -> Message {
    Message {
        id: row.id,
        channel_id: row.channel_id.clone(),
        author: User {
            id: row.author_id,
            username: row.username,
            global_name: None,
            avatar: None,
            bot: false,
        },
        content: row.content,
        timestamp: row.created_at,
        edited_timestamp: row.edited_at,
        message_reference: row.reply_to_id.map(|reply| MessageReference {
            message_id: Some(reply),
            channel_id: Some(row.channel_id),
            guild_id: row.guild_id,
        }),
        kind: 0,
        attachments: Vec::new(),
        reactions: Vec::new(),
        poll: None,
    }
}

pub const MESSAGE_SELECT: &str = "SELECT m.id, m.channel_id, m.author_id, u.username, m.content, m.created_at, m.edited_at, \
     m.reply_to_id, c.guild_id FROM messages m JOIN users u ON u.id = m.author_id JOIN channels c ON c.id = m.channel_id";

pub async fn message_by_id(state: &AppState, message_id: &str) -> ApiResult<MessageRow> {
    sqlx::query_as(&format!("{MESSAGE_SELECT} WHERE m.id = ?"))
        .bind(message_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?
        .ok_or_else(|| missing("message"))
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn random_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let token = auth::new_token();
    token
        .bytes()
        .take(8)
        .map(|b| ALPHABET[(b as usize) % ALPHABET.len()] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// Guilds
// ---------------------------------------------------------------------------

async fn create_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateGuild>,
) -> Response {
    match create_guild_inner(&state, &headers, req).await {
        Ok(v) => v,
        Err(e) => e,
    }
}

async fn create_guild_inner(
    state: &AppState,
    headers: &HeaderMap,
    req: CreateGuild,
) -> ApiResult<Response> {
    let user = authed(state, headers).await?;
    let name = check_name(&req.name)?;
    let guild_id = auth::new_id();
    let general_id = auth::new_id();
    let now = now_iso();

    let mut tx = state.db.begin().await.map_err(db)?;
    sqlx::query("INSERT INTO guilds (id, name, owner_id, created_at) VALUES (?, ?, ?, ?)")
        .bind(&guild_id)
        .bind(&name)
        .bind(&user.id)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    sqlx::query("INSERT INTO guild_members (guild_id, user_id, joined_at) VALUES (?, ?, ?)")
        .bind(&guild_id)
        .bind(&user.id)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    sqlx::query("INSERT INTO channels (id, guild_id, kind, name, position, created_at) VALUES (?, ?, ?, 'general', 0, ?)")
        .bind(&general_id).bind(&guild_id).bind(channel_type::GUILD_TEXT as i64).bind(&now)
        .execute(&mut *tx).await.map_err(db)?;
    sqlx::query("INSERT INTO roles (id, guild_id, name, permissions, color, hoist, mentionable, position, created_at) VALUES (?, ?, '@everyone', ?, 0, 0, 0, 0, ?)")
        .bind(&guild_id)
        .bind(&guild_id)
        .bind(crate::perms::DEFAULT_EVERYONE.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;

    let guild = guild_json(
        state,
        GuildRow {
            id: guild_id,
            name,
            owner_id: user.id.clone(),
        },
        true,
    )
    .await?;
    state.publish(
        vec![user.id.clone()],
        "GUILD_CREATE",
        serde_json::to_value(&guild).unwrap_or_default(),
    );
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

async fn get_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
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

async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        require_member(&state, &guild_id, &user.id).await?;
        Ok(Json(guild_channels(&state, &guild_id).await?).into_response())
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
        crate::perms::require_guild(&state, &guild_id, &user.id, crate::perms::MANAGE_CHANNELS).await?;
        let name = check_name(&req.name)?;
        let kind = req.kind.unwrap_or(channel_type::GUILD_TEXT);
        // Announcement, stage and forum channels are stored like text channels; their extra
        // behavior (posts, threads, stage sessions) is not implemented yet.
        if !matches!(kind, channel_type::GUILD_TEXT | channel_type::GUILD_VOICE | channel_type::GUILD_CATEGORY | 5 | 13 | 15) {
            return Err(api_error(StatusCode::BAD_REQUEST, 50024, "unsupported channel type"));
        }
        // Categories group channels and cannot sit inside another category.
        let parent = match req.parent_id.as_deref().filter(|p| !p.is_empty()) {
            Some(_) if kind == channel_type::GUILD_CATEGORY => {
                return Err(api_error(StatusCode::BAD_REQUEST, 50035, "a category cannot have a parent"));
            }
            Some(parent) => {
                crate::channels::check_parent(&state, &guild_id, parent).await?;
                Some(parent.to_string())
            }
            None => None,
        };
        let channel_id = auth::new_id();
        let position: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM channels WHERE guild_id = ?")
            .bind(&guild_id)
            .fetch_one(&state.db)
            .await
            .map_err(db)?;
        sqlx::query("INSERT INTO channels (id, guild_id, kind, name, topic, position, parent_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&channel_id).bind(&guild_id).bind(kind as i64).bind(&name)
            .bind(req.topic.as_deref().filter(|t| !t.is_empty())).bind(position).bind(parent).bind(now_iso())
            .execute(&state.db).await.map_err(db)?;
        if let Some(overwrites) = &req.permission_overwrites {
            crate::perms::require_guild(&state, &guild_id, &user.id, crate::perms::MANAGE_ROLES).await?;
            crate::channels::write_overwrites(&state, &guild_id, &channel_id, overwrites).await?;
        }
        let channel = channel_json(&state, channel_row(&state, &channel_id).await?).await?;
        let targets = member_ids(&state, &guild_id).await.map_err(db)?;
        state.publish(targets, "CHANNEL_CREATE", serde_json::to_value(&channel).unwrap_or_default());
        Ok((StatusCode::CREATED, Json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn get_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_channel(&state, &channel, &user.id).await?;
        Ok(Json(channel_json(&state, channel).await?).into_response())
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
        require_channel(&state, &channel, &user.id).await?;
        crate::perms::require_channel_row(&state, &channel, &user.id, crate::perms::VIEW_CHANNEL).await?;
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
        let mut messages: Vec<Message> = rows.into_iter().map(message_json).collect();
        uploads::hydrate(&state, &mut messages).await.map_err(db)?;
        crate::reactions::hydrate(&state, &user.id, &mut messages).await.map_err(db)?;
        crate::polls::hydrate(&state, &user.id, &mut messages).await.map_err(db)?;
        Ok(Json(messages).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    req: axum::extract::Request,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_channel(&state, &channel, &user.id).await?;
        crate::perms::require_channel_row(&state, &channel, &user.id, crate::perms::VIEW_CHANNEL | crate::perms::SEND_MESSAGES).await?;

        let is_multipart = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("multipart/form-data"));
        let (payload, files) = if is_multipart {
            read_multipart(&state, req).await?
        } else {
            (read_json(req).await?, Vec::new())
        };

        // Staged uploads from the signed-upload flow, checked before anything is written.
        let claimed = if payload.attachments.is_empty() {
            Vec::new()
        } else {
            let tokens: Vec<String> = payload.attachments.iter().map(|a| a.uploaded_filename.clone()).collect();
            uploads::claim(&state, &channel_id, &user.id, &tokens).await?
        };

        let content = payload.content.trim_end().to_string();
        if content.chars().count() > MAX_CONTENT {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("content must be at most {MAX_CONTENT} characters")));
        }
        if let Some(poll) = &payload.poll {
            crate::polls::validate(poll)?;
        }
        if content.trim().is_empty() && files.is_empty() && claimed.is_empty() && payload.poll.is_none() {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "a message needs content or a file"));
        }
        let reply_to = match payload.message_reference.as_ref().and_then(|r| r.message_id.clone()) {
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
        if let Some(poll) = &payload.poll {
            if let Err(e) = crate::polls::create(&state, &message_id, poll).await {
                let _ = sqlx::query("DELETE FROM messages WHERE id = ?").bind(&message_id).execute(&state.db).await;
                return Err(db(e));
            }
        }
        if !claimed.is_empty() {
            uploads::attach_claimed(&state, &channel_id, &message_id, &claimed).await.map_err(db)?;
        }
        for file in files {
            if let Err(e) = uploads::store(&state, &channel_id, &message_id, file).await {
                // Do not leave a half-sent message behind.
                let _ = uploads::remove_for_message(&state, &message_id).await;
                let _ = sqlx::query("DELETE FROM messages WHERE id = ?").bind(&message_id).execute(&state.db).await;
                return Err(e);
            }
        }

        let mut out = vec![message_json(message_by_id(&state, &message_id).await?)];
        uploads::hydrate(&state, &mut out).await.map_err(db)?;
        crate::reactions::hydrate(&state, &user.id, &mut out).await.map_err(db)?;
        crate::polls::hydrate(&state, &user.id, &mut out).await.map_err(db)?;
        let message = out.remove(0);
        let targets = channel_audience(&state, &channel).await.map_err(db)?;
        state.publish(targets, "MESSAGE_CREATE", serde_json::to_value(&message).unwrap_or_default());
        Ok((StatusCode::OK, Json(message)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// JSON body: a plain `CreateMessage`.
async fn read_json(req: axum::extract::Request) -> ApiResult<CreateMessage> {
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map_err(|_| {
            api_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                50035,
                "request body too large",
            )
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, 50109, "invalid JSON body"))
}

/// Multipart body: `payload_json` holds the message, `files[n]` hold the attachments.
async fn read_multipart(
    state: &AppState,
    req: axum::extract::Request,
) -> ApiResult<(CreateMessage, Vec<PendingFile>)> {
    let bad = |m: &str| api_error(StatusCode::BAD_REQUEST, 50035, m.to_string());
    let mut multipart = axum::extract::Multipart::from_request(req, state)
        .await
        .map_err(|_| bad("invalid multipart body"))?;
    let mut payload: Option<CreateMessage> = None;
    let mut files = Vec::new();
    let max_bytes = uploads::max_file_bytes();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| bad("invalid multipart body"))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "payload_json" {
            let text = field
                .text()
                .await
                .map_err(|_| bad("invalid payload_json"))?;
            payload =
                Some(serde_json::from_str(&text).map_err(|_| bad("payload_json is not valid"))?);
        } else if name.starts_with("files") {
            if files.len() >= MAX_FILES {
                return Err(bad("too many files on one message"));
            }
            let filename = field.file_name().unwrap_or("file").to_string();
            let content_type = field.content_type().map(|c| c.to_string());
            let bytes = field.bytes().await.map_err(|_| bad("invalid file part"))?;
            if bytes.len() as u64 > max_bytes {
                return Err(api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    40005,
                    "file is too large",
                ));
            }
            files.push(PendingFile {
                filename,
                content_type,
                bytes: bytes.to_vec(),
            });
        }
    }
    Ok((
        payload.unwrap_or(CreateMessage {
            content: String::new(),
            message_reference: None,
            attachments: Vec::new(),
            poll: None,
        }),
        files,
    ))
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
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                50035,
                format!("content must be 1-{MAX_CONTENT} characters"),
            ));
        }
        sqlx::query("UPDATE messages SET content = ?, edited_at = ? WHERE id = ?")
            .bind(&content)
            .bind(now_iso())
            .bind(&message_id)
            .execute(&state.db)
            .await
            .map_err(db)?;
        let mut out = vec![message_json(message_by_id(&state, &message_id).await?)];
        uploads::hydrate(&state, &mut out).await.map_err(db)?;
        crate::reactions::hydrate(&state, &user.id, &mut out)
            .await
            .map_err(db)?;
        crate::polls::hydrate(&state, &user.id, &mut out)
            .await
            .map_err(db)?;
        let message = out.remove(0);
        let channel = channel_row(&state, &channel_id).await?;
        let targets = channel_audience(&state, &channel).await.map_err(db)?;
        state.publish(
            targets,
            "MESSAGE_UPDATE",
            serde_json::to_value(&message).unwrap_or_default(),
        );
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
        let channel = channel_row(&state, &channel_id).await?;
        let is_guild_owner = match &channel.guild_id {
            Some(guild_id) => guild_row(&state, guild_id).await?.owner_id == user.id,
            None => false,
        };
        if existing.author_id != user.id && !is_guild_owner {
            return Err(no_access());
        }
        uploads::remove_for_message(&state, &message_id)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM messages WHERE id = ?")
            .bind(&message_id)
            .execute(&state.db)
            .await
            .map_err(db)?;
        let targets = channel_audience(&state, &channel).await.map_err(db)?;
        state.publish(
            targets,
            "MESSAGE_DELETE",
            json!({ "id": message_id, "channel_id": channel_id, "guild_id": channel.guild_id }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn typing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_channel(&state, &channel, &user.id).await?;
        let targets = channel_audience(&state, &channel).await.map_err(db)?;
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

async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let channel = channel_row(&state, &channel_id).await?;
        require_channel(&state, &channel, &user.id).await?;
        crate::perms::require_channel_row(&state, &channel, &user.id, crate::perms::CREATE_INSTANT_INVITE).await?;
        let guild_id = channel.guild_id.clone().ok_or_else(|| {
            api_error(StatusCode::BAD_REQUEST, 50035, "invites are only for guild channels")
        })?;
        let code = random_code();
        sqlx::query("INSERT INTO invites (code, guild_id, channel_id, inviter_id, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&code).bind(&guild_id).bind(&channel_id).bind(&user.id).bind(now_iso())
            .execute(&state.db).await.map_err(db)?;
        let guild = guild_json(&state, guild_row(&state, &guild_id).await?, false).await?;
        let channel = channel_json(&state, channel).await?;
        Ok((StatusCode::OK, Json(Invite { code, guild, channel })).into_response())
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
        let channel = channel_json(&state, channel_row(&state, &channel_id).await?).await?;
        Ok(Json(Invite {
            code,
            guild,
            channel,
        })
        .into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let (guild_id, _channel_id) = invite_row(&state, &code).await?;
        let banned: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM guild_bans WHERE guild_id = ? AND user_id = ?")
                .bind(&guild_id)
                .bind(&user.id)
                .fetch_optional(&state.db)
                .await
                .map_err(db)?;
        if banned.is_some() {
            return Err(no_access());
        }
        sqlx::query(
            "INSERT OR IGNORE INTO guild_members (guild_id, user_id, joined_at) VALUES (?, ?, ?)",
        )
        .bind(&guild_id)
        .bind(&user.id)
        .bind(now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        let guild = guild_json(&state, guild_row(&state, &guild_id).await?, true).await?;
        state.publish(
            vec![user.id.clone()],
            "GUILD_CREATE",
            serde_json::to_value(&guild).unwrap_or_default(),
        );
        let members = member_ids(&state, &guild_id).await.map_err(db)?;
        let member = json!({ "guild_id": guild_id, "user": user });
        let others: Vec<String> = members.into_iter().filter(|m| *m != user.id).collect();
        state.publish(others, "GUILD_MEMBER_ADD", member);
        Ok(Json(guild).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

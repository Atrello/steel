//! Public threads. A thread is a channel of type 11 whose `parent_id` is the text channel it was started in.
//! A thread started from a message takes that message's id. Steel does not have thread members, private
//! threads, or archiving yet: threads stay open, and the archived lists are empty.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use flint_protocol::{Channel, CreateThread, ThreadMetadata};
use serde_json::json;

use crate::{
    api::{api_error, API_PREFIX},
    auth,
    chat::{self, ApiResult, ChannelRow},
    state::AppState,
};

pub const PUBLIC_THREAD: u8 = 11;
const TEXT_PARENTS: [i64; 2] = [0, 5];
/// Auto-archive windows in minutes that clients may ask for.
const ARCHIVE_WINDOWS: [u32; 4] = [60, 1440, 4320, 10080];
const DEFAULT_ARCHIVE: u32 = 1440;
const MAX_NAME_CHARS: usize = 100;
/// Discord's code for "a thread already exists for this message".
const THREAD_ALREADY_EXISTS: u32 = 160004;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/{{message_id}}/threads"),
            post(create_from_message),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/threads"),
            post(create_standalone),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/threads/active"),
            get(active),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/threads/archived/public"),
            get(archived_public),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/threads/archived/private"),
            get(private_forbidden),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/users/@me/threads/archived/private"),
            get(empty_page),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

fn invalid(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

/// Thread metadata for a thread channel, or None when the channel is not a thread.
pub async fn metadata(
    state: &AppState,
    channel_id: &str,
) -> Result<Option<ThreadMetadata>, sqlx::Error> {
    let row: Option<(i64, i64, String)> = sqlx::query_as(&crate::db::sql(
        "SELECT archived, auto_archive_minutes, archive_timestamp FROM threads WHERE channel_id = ?",
    ))
    .bind(channel_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(archived, minutes, at)| ThreadMetadata {
        archived: archived != 0,
        auto_archive_duration: minutes as u32,
        archive_timestamp: at,
    }))
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid("thread name must be 1-100 characters"));
    }
    Ok(name.to_string())
}

fn check_archive(minutes: Option<u32>) -> ApiResult<u32> {
    match minutes {
        None => Ok(DEFAULT_ARCHIVE),
        Some(m) if ARCHIVE_WINDOWS.contains(&m) => Ok(m),
        Some(_) => Err(invalid(
            "auto_archive_duration must be 60, 1440, 4320 or 10080",
        )),
    }
}

/// The text channel a thread belongs to. Threads only start in guild text channels and announcement channels.
async fn text_parent(
    state: &AppState,
    headers: &HeaderMap,
    channel_id: &str,
) -> ApiResult<(String, ChannelRow)> {
    let user = chat::authed(state, headers).await?;
    let parent = chat::channel_row(state, channel_id).await?;
    chat::require_channel(state, &parent, &user.id).await?;
    if parent.guild_id.is_none() || !TEXT_PARENTS.contains(&parent.kind) {
        return Err(invalid("threads can only start in a text channel"));
    }
    Ok((user.id, parent))
}

/// Writes the thread rows, then sends THREAD_CREATE to the guild and returns the channel.
async fn insert_thread(
    state: &AppState,
    id: &str,
    parent: &ChannelRow,
    owner_id: &str,
    name: &str,
    archive_minutes: u32,
) -> ApiResult<Channel> {
    let guild_id = parent.guild_id.clone().unwrap_or_default();
    let now = chat::now_iso();
    sqlx::query(&crate::db::sql(
        "INSERT INTO channels (id, guild_id, kind, name, topic, position, parent_id, created_at) VALUES (?, ?, ?, ?, NULL, 0, ?, ?)",
    ))
    .bind(id)
    .bind(&guild_id)
    .bind(i64::from(PUBLIC_THREAD))
    .bind(name)
    .bind(&parent.id)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(db)?;
    sqlx::query(&crate::db::sql(
        "INSERT INTO threads (channel_id, owner_id, archived, auto_archive_minutes, archive_timestamp, created_at) VALUES (?, ?, 0, ?, ?, ?)",
    ))
    .bind(id)
    .bind(owner_id)
    .bind(i64::from(archive_minutes))
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(db)?;

    let row = chat::channel_row(state, id).await?;
    let channel = chat::channel_json(state, row).await?;
    let targets = chat::member_ids(state, &guild_id).await.map_err(db)?;
    state.publish(
        targets,
        "THREAD_CREATE",
        serde_json::to_value(&channel).unwrap_or_default(),
    );
    Ok(channel)
}

/// `POST /channels/{id}/messages/{id}/threads`: one thread per message.
async fn create_from_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
    Json(body): Json<CreateThread>,
) -> Response {
    let result: ApiResult<Response> = async {
        let (user_id, parent) = text_parent(&state, &headers, &channel_id).await?;
        let name = check_name(&body.name)?;
        let archive = check_archive(body.auto_archive_duration)?;
        let message = chat::message_by_id(&state, &message_id).await?;
        if message.channel_id != channel_id {
            return Err(chat::missing_message());
        }
        let exists: Option<(i64,)> =
            sqlx::query_as(&crate::db::sql("SELECT 1 FROM channels WHERE id = ?"))
                .bind(&message_id)
                .fetch_optional(&state.db)
                .await
                .map_err(db)?;
        if exists.is_some() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                THREAD_ALREADY_EXISTS,
                "A thread has already been created for this message",
            ));
        }
        let channel = insert_thread(&state, &message_id, &parent, &user_id, &name, archive).await?;
        Ok((StatusCode::CREATED, Json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `POST /channels/{id}/threads`: a thread not tied to a message. Only public threads (type 11) are supported.
async fn create_standalone(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(body): Json<CreateThread>,
) -> Response {
    let result: ApiResult<Response> = async {
        let (user_id, parent) = text_parent(&state, &headers, &channel_id).await?;
        if body.kind.unwrap_or(PUBLIC_THREAD) != PUBLIC_THREAD {
            return Err(invalid("only public threads are supported"));
        }
        let name = check_name(&body.name)?;
        let archive = check_archive(body.auto_archive_duration)?;
        let id = auth::new_id();
        let channel = insert_thread(&state, &id, &parent, &user_id, &name, archive).await?;
        Ok((StatusCode::CREATED, Json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn threads_json(state: &AppState, rows: Vec<ChannelRow>) -> ApiResult<Response> {
    let mut threads = Vec::with_capacity(rows.len());
    for row in rows {
        threads.push(chat::channel_json(state, row).await?);
    }
    Ok(Json(json!({ "threads": threads, "members": [], "has_more": false })).into_response())
}

const THREAD_ROWS: &str = "SELECT c.id, c.guild_id, c.kind, c.name, c.topic, c.position, c.parent_id FROM channels c JOIN threads t ON t.channel_id = c.id";

/// `GET /guilds/{id}/threads/active`: every open thread in the guild.
async fn active(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let rows: Vec<ChannelRow> = sqlx::query_as(&crate::db::sql(&format!(
            "{THREAD_ROWS} WHERE c.guild_id = ? AND t.archived = 0 ORDER BY c.created_at, c.id"
        )))
        .bind(&guild_id)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        threads_json(&state, rows).await
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `GET /channels/{id}/threads/archived/public`. Steel does not archive threads yet, so this is empty.
async fn archived_public(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let parent = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &parent, &user.id).await?;
        let rows: Vec<ChannelRow> = sqlx::query_as(&crate::db::sql(&format!(
            "{THREAD_ROWS} WHERE c.parent_id = ? AND t.archived = 1 ORDER BY c.created_at, c.id"
        )))
        .bind(&channel_id)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        threads_json(&state, rows).await
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Private threads are not supported, so listing them is refused, as for a member without permission.
async fn private_forbidden() -> Response {
    api_error(StatusCode::FORBIDDEN, 50013, "Missing Permissions").into_response()
}

async fn empty_page() -> Response {
    Json(json!({ "threads": [], "members": [], "has_more": false })).into_response()
}

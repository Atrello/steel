//! Guild stickers. Members list them; managers create, edit and delete them (`MANAGE_GUILD`).
//! Files are PNG or GIF, stored on disk and served from `/cdn/stickers/{id}.{ext}`.
//! Every change sends GUILD_STICKERS_UPDATE with the full list, which is how Flint refreshes its picker.

use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch},
    Json, Router,
};
use flint_protocol::{Message, StickerItem};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    api::{api_error, API_PREFIX},
    auth,
    chat::{self, ApiResult},
    perms,
    state::AppState,
    uploads,
};

pub const MAX_STICKER_BYTES: usize = 512 * 1024;
pub const MAX_STICKERS_PER_GUILD: i64 = 50;
const NAME_MIN: usize = 2;
const NAME_MAX: usize = 30;
const DESCRIPTION_MAX: usize = 100;
const TAGS_MAX: usize = 200;

const FORMAT_PNG: i64 = 1;
const FORMAT_GIF: i64 = 4;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/stickers"),
            get(list).post(create),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/stickers/{{sticker_id}}"),
            patch(edit).delete(remove),
        )
        .route("/cdn/stickers/{file}", get(serve))
}

#[derive(Deserialize)]
pub struct EditSticker {
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Option<String>,
}

fn db(e: sqlx::Error) -> Response {
    chat::db(e)
}

fn invalid(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

fn check_fields(name: &str, description: &str, tags: &str) -> ApiResult<()> {
    let name_len = name.chars().count();
    if !(NAME_MIN..=NAME_MAX).contains(&name_len) || name.chars().any(char::is_control) {
        return Err(invalid("sticker name must be 2-30 characters"));
    }
    let description_len = description.chars().count();
    if !description.is_empty() && !(NAME_MIN..=DESCRIPTION_MAX).contains(&description_len) {
        return Err(invalid("sticker description must be 2-100 characters"));
    }
    let tags_len = tags.chars().count();
    if !(1..=TAGS_MAX).contains(&tags_len) || tags.chars().any(char::is_control) {
        return Err(invalid("sticker tags must be 1-200 characters"));
    }
    Ok(())
}

/// Recognizes PNG and GIF by their signatures. Returns the Discord format type and file extension.
fn sniff(bytes: &[u8]) -> ApiResult<(i64, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok((FORMAT_PNG, "png"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok((FORMAT_GIF, "gif"))
    } else {
        Err(invalid("sticker file must be a PNG or GIF"))
    }
}

fn sticker_json(
    guild_id: &str,
    id: &str,
    name: &str,
    description: &str,
    tags: &str,
    format: i64,
) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": if description.is_empty() { Value::Null } else { json!(description) },
        "tags": tags,
        "type": 2,
        "format_type": format,
        "guild_id": guild_id,
        "available": true,
    })
}

type Row = (String, String, String, String, i64);

async fn sticker_rows(state: &AppState, guild_id: &str) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, name, description, tags, format_type FROM guild_stickers WHERE guild_id = ? ORDER BY LOWER(name), id",
    ))
    .bind(guild_id)
    .fetch_all(&state.db)
    .await
}

/// Sends GUILD_STICKERS_UPDATE with the full list to every member of the guild.
async fn publish_list(state: &AppState, guild_id: &str) -> ApiResult<Vec<Value>> {
    let items: Vec<Value> = sticker_rows(state, guild_id)
        .await
        .map_err(db)?
        .iter()
        .map(|(id, name, description, tags, format)| {
            sticker_json(guild_id, id, name, description, tags, *format)
        })
        .collect();
    let targets = chat::member_ids(state, guild_id).await.map_err(db)?;
    state.publish(
        targets,
        "GUILD_STICKERS_UPDATE",
        json!({"guild_id": guild_id, "stickers": items}),
    );
    Ok(items)
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let items: Vec<Value> = sticker_rows(&state, &guild_id)
            .await
            .map_err(db)?
            .iter()
            .map(|(id, name, description, tags, format)| {
                sticker_json(&guild_id, id, name, description, tags, *format)
            })
            .collect();
        Ok(Json(items).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    mut multipart: Multipart,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;

        let mut name = None;
        let mut description = String::new();
        let mut tags = None;
        let mut file = None;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|_| invalid("malformed multipart body"))?
        {
            match field.name().unwrap_or_default().to_string().as_str() {
                "name" => name = Some(field.text().await.map_err(|_| invalid("bad name"))?),
                "description" => {
                    description = field.text().await.map_err(|_| invalid("bad description"))?
                }
                "tags" => tags = Some(field.text().await.map_err(|_| invalid("bad tags"))?),
                "file" => {
                    let bytes = field.bytes().await.map_err(|_| invalid("bad file"))?;
                    if bytes.is_empty() || bytes.len() > MAX_STICKER_BYTES {
                        return Err(invalid("sticker file must be at most 512 KiB"));
                    }
                    file = Some(bytes.to_vec());
                }
                _ => {}
            }
        }
        let name = name.ok_or_else(|| invalid("sticker name is required"))?.trim().to_string();
        let tags = tags.ok_or_else(|| invalid("sticker tags are required"))?.trim().to_string();
        let description = description.trim().to_string();
        check_fields(&name, &description, &tags)?;
        let bytes = file.ok_or_else(|| invalid("sticker file is required"))?;
        let (format, ext) = sniff(&bytes)?;

        let count: i64 = sqlx::query_scalar(&crate::db::sql(
            "SELECT COUNT(*) FROM guild_stickers WHERE guild_id = ?",
        ))
        .bind(&guild_id)
        .fetch_one(&state.db)
        .await
        .map_err(db)?;
        if count >= MAX_STICKERS_PER_GUILD {
            return Err(invalid("this server has reached its sticker limit"));
        }
        let taken: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_stickers WHERE guild_id = ? AND name = ?",
        ))
        .bind(&guild_id)
        .bind(&name)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if taken.is_some() {
            return Err(invalid("a sticker with this name already exists"));
        }

        let id = auth::new_id();
        let dir = uploads::data_dir().join("stickers").join(&guild_id);
        tokio::fs::create_dir_all(&dir).await.map_err(crate::api::internal)?;
        tokio::fs::write(dir.join(format!("{id}.{ext}")), &bytes)
            .await
            .map_err(crate::api::internal)?;
        sqlx::query(&crate::db::sql(
            "INSERT INTO guild_stickers (id, guild_id, name, description, tags, format_type, ext, creator_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&id)
        .bind(&guild_id)
        .bind(&name)
        .bind(&description)
        .bind(&tags)
        .bind(format)
        .bind(ext)
        .bind(&user.id)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        publish_list(&state, &guild_id).await?;
        Ok((
            StatusCode::CREATED,
            Json(sticker_json(&guild_id, &id, &name, &description, &tags, format)),
        )
            .into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, sticker_id)): Path<(String, String)>,
    Json(body): Json<EditSticker>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let current: Option<Row> = sqlx::query_as(&crate::db::sql(
            "SELECT id, name, description, tags, format_type FROM guild_stickers WHERE guild_id = ? AND id = ?",
        ))
        .bind(&guild_id)
        .bind(&sticker_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let Some((_, name, description, tags, format)) = current else {
            return Err(api_error(StatusCode::NOT_FOUND, 10060, "Unknown Sticker"));
        };
        let name = body.name.map(|n| n.trim().to_string()).unwrap_or(name);
        let description = body.description.map(|d| d.trim().to_string()).unwrap_or(description);
        let tags = body.tags.map(|t| t.trim().to_string()).unwrap_or(tags);
        check_fields(&name, &description, &tags)?;

        let taken: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_stickers WHERE guild_id = ? AND name = ? AND id <> ?",
        ))
        .bind(&guild_id)
        .bind(&name)
        .bind(&sticker_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if taken.is_some() {
            return Err(invalid("a sticker with this name already exists"));
        }
        sqlx::query(&crate::db::sql(
            "UPDATE guild_stickers SET name = ?, description = ?, tags = ? WHERE guild_id = ? AND id = ?",
        ))
        .bind(&name)
        .bind(&description)
        .bind(&tags)
        .bind(&guild_id)
        .bind(&sticker_id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        publish_list(&state, &guild_id).await?;
        Ok(Json(sticker_json(&guild_id, &sticker_id, &name, &description, &tags, format)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, sticker_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let row: Option<(String,)> = sqlx::query_as(&crate::db::sql(
            "SELECT ext FROM guild_stickers WHERE guild_id = ? AND id = ?",
        ))
        .bind(&guild_id)
        .bind(&sticker_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let Some((ext,)) = row else {
            return Err(api_error(StatusCode::NOT_FOUND, 10060, "Unknown Sticker"));
        };
        sqlx::query(&crate::db::sql(
            "DELETE FROM guild_stickers WHERE guild_id = ? AND id = ?",
        ))
        .bind(&guild_id)
        .bind(&sticker_id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        let _ = tokio::fs::remove_file(
            uploads::data_dir()
                .join("stickers")
                .join(&guild_id)
                .join(format!("{sticker_id}.{ext}")),
        )
        .await;
        publish_list(&state, &guild_id).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn serve(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    let not_found = || api_error(StatusCode::NOT_FOUND, 10060, "Unknown Sticker").into_response();
    let Some((id, ext)) = file.rsplit_once('.') else {
        return not_found();
    };
    let row: Option<(String, String)> = match sqlx::query_as(&crate::db::sql(
        "SELECT guild_id, ext FROM guild_stickers WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => return db(e),
    };
    let Some((guild_id, stored_ext)) = row.filter(|(_, e)| e == ext) else {
        return not_found();
    };
    let content_type = if stored_ext == "gif" {
        "image/gif"
    } else {
        "image/png"
    };
    match tokio::fs::read(
        uploads::data_dir()
            .join("stickers")
            .join(&guild_id)
            .join(format!("{id}.{stored_ext}")),
    )
    .await
    {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/// Largest number of stickers one message may carry.
pub const MAX_MESSAGE_STICKERS: usize = 3;

/// Checks the sticker ids a message wants to carry. Each must be a sticker of the channel's guild.
pub async fn check_send(
    state: &AppState,
    guild_id: &str,
    ids: &[String],
) -> ApiResult<Vec<String>> {
    if ids.len() > MAX_MESSAGE_STICKERS {
        return Err(invalid("a message can carry at most 3 stickers"));
    }
    for id in ids {
        let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_stickers WHERE guild_id = ? AND id = ?",
        ))
        .bind(guild_id)
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if found.is_none() {
            return Err(invalid("unknown sticker"));
        }
    }
    Ok(ids.to_vec())
}

/// Records the stickers of a new message, in order.
pub async fn attach(state: &AppState, message_id: &str, ids: &[String]) -> Result<(), sqlx::Error> {
    for (position, id) in ids.iter().enumerate() {
        sqlx::query(&crate::db::sql(
            "INSERT INTO message_stickers (message_id, position, sticker_id) VALUES (?, ?, ?)",
        ))
        .bind(message_id)
        .bind(position as i64)
        .bind(id)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// Fills `sticker_items` on each message. Stickers deleted since sending are left out.
pub async fn hydrate_messages(
    state: &AppState,
    messages: &mut [Message],
) -> Result<(), sqlx::Error> {
    for message in messages.iter_mut() {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(&crate::db::sql(
            "SELECT gs.id, gs.name, gs.format_type FROM message_stickers ms \
             JOIN guild_stickers gs ON gs.id = ms.sticker_id \
             WHERE ms.message_id = ? ORDER BY ms.position",
        ))
        .bind(&message.id)
        .fetch_all(&state.db)
        .await?;
        message.sticker_items = rows
            .into_iter()
            .map(|(id, name, format)| StickerItem {
                id,
                name,
                format_type: format as u8,
            })
            .collect();
    }
    Ok(())
}

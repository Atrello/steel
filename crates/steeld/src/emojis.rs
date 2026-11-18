//! Custom guild emoji. Managers add, rename and delete emoji (`MANAGE_GUILD`). Members list them.
//! Images are PNG or GIF data URIs, stored on disk and served from `/cdn/emojis/{id}.{ext}`.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
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

pub const MAX_EMOJI_BYTES: usize = 256 * 1024;
pub const MAX_EMOJI_PER_GUILD: i64 = 50;
const NAME_MIN: usize = 2;
const NAME_MAX: usize = 32;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/emojis"),
            get(list).post(create),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/emojis/{{emoji_id}}"),
            patch(rename).delete(remove),
        )
        .route("/cdn/emojis/{file}", get(serve))
}

#[derive(Deserialize)]
pub struct CreateEmoji {
    pub name: String,
    pub image: String,
}

#[derive(Deserialize)]
pub struct RenameEmoji {
    pub name: String,
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

fn invalid(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

fn check_name(name: &str) -> ApiResult<String> {
    let len = name.chars().count();
    if !(NAME_MIN..=NAME_MAX).contains(&len)
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(invalid(
            "emoji name must be 2-32 letters, digits or underscores",
        ));
    }
    Ok(name.to_string())
}

/// Decodes a PNG or GIF data URI. Returns the file extension, whether it is animated, and the bytes.
fn decode_image(uri: &str) -> ApiResult<(&'static str, bool, Vec<u8>)> {
    let (ext, animated, data) = if let Some(d) = uri.strip_prefix("data:image/png;base64,") {
        ("png", false, d)
    } else if let Some(d) = uri.strip_prefix("data:image/gif;base64,") {
        ("gif", true, d)
    } else {
        return Err(invalid("emoji image must be a PNG or GIF data URI"));
    };
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| invalid("emoji image is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_EMOJI_BYTES {
        return Err(invalid("emoji image must be at most 256 KiB"));
    }
    Ok((ext, animated, bytes))
}

fn emoji_json(id: &str, name: &str, animated: i64) -> Value {
    json!({
        "id": id,
        "name": name,
        "animated": animated != 0,
        "available": true,
        "managed": false,
        "roles": null,
        "user": null,
        "require_colons": true,
    })
}

async fn emoji_rows(
    state: &AppState,
    guild_id: &str,
) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, name, animated FROM guild_emojis WHERE guild_id = ? ORDER BY LOWER(name), id",
    ))
    .bind(guild_id)
    .fetch_all(&state.db)
    .await
}

/// Sends GUILD_EMOJIS_UPDATE with the full list, which is how Flint refreshes the emoji picker.
async fn publish_list(state: &AppState, guild_id: &str) -> ApiResult<Vec<Value>> {
    let items: Vec<Value> = emoji_rows(state, guild_id)
        .await
        .map_err(db)?
        .iter()
        .map(|(id, name, animated)| emoji_json(id, name, *animated))
        .collect();
    let targets = chat::member_ids(state, guild_id).await.map_err(db)?;
    state.publish(
        targets,
        "GUILD_EMOJIS_UPDATE",
        json!({"guild_id": guild_id, "emojis": items}),
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
        let items: Vec<Value> = emoji_rows(&state, &guild_id)
            .await
            .map_err(db)?
            .iter()
            .map(|(id, name, animated)| emoji_json(id, name, *animated))
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
    Json(body): Json<CreateEmoji>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let name = check_name(&body.name)?;
        let (ext, animated, bytes) = decode_image(&body.image)?;
        let count: i64 = sqlx::query_scalar(&crate::db::sql("SELECT COUNT(*) FROM guild_emojis WHERE guild_id = ?"))
            .bind(&guild_id)
            .fetch_one(&state.db)
            .await
            .map_err(db)?;
        if count >= MAX_EMOJI_PER_GUILD {
            return Err(invalid("this server has reached its emoji limit"));
        }
        let taken: Option<(i64,)> = sqlx::query_as(&crate::db::sql("SELECT 1 FROM guild_emojis WHERE guild_id = ? AND name = ?"))
            .bind(&guild_id)
            .bind(&name)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
        if taken.is_some() {
            return Err(invalid("an emoji with this name already exists"));
        }
        let id = auth::new_id();
        let dir = uploads::data_dir().join("emojis").join(&guild_id);
        tokio::fs::create_dir_all(&dir).await.map_err(crate::api::internal)?;
        tokio::fs::write(dir.join(format!("{id}.{ext}")), &bytes)
            .await
            .map_err(crate::api::internal)?;
        sqlx::query(&crate::db::sql(
            "INSERT INTO guild_emojis (id, guild_id, name, animated, ext, creator_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&id)
        .bind(&guild_id)
        .bind(&name)
        .bind(i64::from(animated))
        .bind(ext)
        .bind(&user.id)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        publish_list(&state, &guild_id).await?;
        Ok((StatusCode::CREATED, Json(emoji_json(&id, &name, i64::from(animated)))).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, emoji_id)): Path<(String, String)>,
    Json(body): Json<RenameEmoji>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let name = check_name(&body.name)?;
        let row: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT animated FROM guild_emojis WHERE id = ? AND guild_id = ?",
        ))
        .bind(&emoji_id)
        .bind(&guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let Some((animated,)) = row else {
            return Err(chat::missing_message());
        };
        let taken: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
            "SELECT 1 FROM guild_emojis WHERE guild_id = ? AND name = ? AND id <> ?",
        ))
        .bind(&guild_id)
        .bind(&name)
        .bind(&emoji_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        if taken.is_some() {
            return Err(invalid("an emoji with this name already exists"));
        }
        sqlx::query(&crate::db::sql(
            "UPDATE guild_emojis SET name = ? WHERE id = ?",
        ))
        .bind(&name)
        .bind(&emoji_id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        publish_list(&state, &guild_id).await?;
        Ok(Json(emoji_json(&emoji_id, &name, animated)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, emoji_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let row: Option<(String,)> = sqlx::query_as(&crate::db::sql(
            "SELECT ext FROM guild_emojis WHERE id = ? AND guild_id = ?",
        ))
        .bind(&emoji_id)
        .bind(&guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let Some((ext,)) = row else {
            return Err(chat::missing_message());
        };
        sqlx::query(&crate::db::sql("DELETE FROM guild_emojis WHERE id = ?"))
            .bind(&emoji_id)
            .execute(&state.db)
            .await
            .map_err(db)?;
        let _ = tokio::fs::remove_file(
            uploads::data_dir()
                .join("emojis")
                .join(&guild_id)
                .join(format!("{emoji_id}.{ext}")),
        )
        .await;
        publish_list(&state, &guild_id).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `GET /cdn/emojis/{id}.{ext}`. Emoji images are public, as Discord's are.
async fn serve(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    let Some((id, ext)) = file.rsplit_once('.') else {
        return api_error(StatusCode::NOT_FOUND, 10014, "Unknown Emoji").into_response();
    };
    let row: Option<(String, String)> = match sqlx::query_as(&crate::db::sql(
        "SELECT guild_id, ext FROM guild_emojis WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => return db(e),
    };
    let Some((guild_id, stored_ext)) = row.filter(|(_, e)| e == ext) else {
        return api_error(StatusCode::NOT_FOUND, 10014, "Unknown Emoji").into_response();
    };
    let content_type = if stored_ext == "gif" {
        "image/gif"
    } else {
        "image/png"
    };
    match tokio::fs::read(
        uploads::data_dir()
            .join("emojis")
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
        Err(_) => api_error(StatusCode::NOT_FOUND, 10014, "Unknown Emoji").into_response(),
    }
}

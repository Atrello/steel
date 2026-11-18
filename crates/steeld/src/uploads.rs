//! File uploads and the CDN. Attachments are stored under `STEEL_DATA_DIR/attachments/`.
//! Attachment IDs are random tokens, so the CDN URL is not guessable. The URL itself does not require a
//! session, because image and file widgets fetch it without headers.

use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use flint_protocol::{Attachment, StageFiles};

use crate::{
    api::{api_error, API_PREFIX},
    auth,
    state::AppState,
};

/// Per-file limit. Override with STEEL_MAX_UPLOAD_BYTES.
pub fn max_file_bytes() -> u64 {
    std::env::var("STEEL_MAX_UPLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100 * 1024 * 1024)
}

/// Files per message.
pub const MAX_FILES: usize = 10;

pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("STEEL_DATA_DIR").unwrap_or_else(|_| "data".into()))
}

/// Public base for CDN URLs (for example `https://chat.example.com`). Empty means relative URLs.
fn public_base() -> String {
    std::env::var("STEEL_PUBLIC_URL")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "file".into()
    } else {
        trimmed.to_string()
    }
}

/// A file read from a multipart request, not yet stored.
pub struct PendingFile {
    pub filename: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// Write a file for `message_id` and record it. Returns the attachment as clients see it.
pub async fn store(
    state: &AppState,
    channel_id: &str,
    message_id: &str,
    file: PendingFile,
) -> Result<Attachment, Response> {
    let id = auth::new_random_numeric_id();
    let filename = sanitize_filename(&file.filename);
    let rel = format!("attachments/{channel_id}/{id}/{filename}");
    let full = data_dir().join(&rel);
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(crate::api::internal)?;
    }
    tokio::fs::write(&full, &file.bytes)
        .await
        .map_err(crate::api::internal)?;
    let size = file.bytes.len() as u64;
    sqlx::query(&crate::db::sql("INSERT INTO attachments (id, message_id, channel_id, filename, content_type, size, path) VALUES (?, ?, ?, ?, ?, ?, ?)"))
        .bind(&id)
        .bind(message_id)
        .bind(channel_id)
        .bind(&filename)
        .bind(&file.content_type)
        .bind(size as i64)
        .bind(&rel)
        .execute(&state.db)
        .await
        .map_err(crate::api::internal)?;
    Ok(attachment_json(
        &id,
        &filename,
        file.content_type,
        size,
        channel_id,
    ))
}

fn attachment_json(
    id: &str,
    filename: &str,
    content_type: Option<String>,
    size: u64,
    channel_id: &str,
) -> Attachment {
    Attachment {
        id: id.to_string(),
        filename: filename.to_string(),
        content_type,
        size,
        url: format!(
            "{}/cdn/attachments/{channel_id}/{id}/{filename}",
            public_base()
        ),
    }
}

/// Attach stored files to messages, in one query.
pub async fn hydrate(
    state: &AppState,
    messages: &mut [flint_protocol::Message],
) -> Result<(), sqlx::Error> {
    if messages.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; messages.len()].join(",");
    let sql = crate::db::sql(&format!(
        "SELECT message_id, id, channel_id, filename, content_type, size FROM attachments WHERE message_id IN ({placeholders}) ORDER BY CAST(id AS BIGINT)"
    ))
    .into_owned();
    let mut q = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(&sql);
    for m in messages.iter() {
        q = q.bind(m.id.clone());
    }
    let rows = q.fetch_all(&state.db).await?;
    for (message_id, id, channel_id, filename, content_type, size) in rows {
        if let Some(m) = messages.iter_mut().find(|m| m.id == message_id) {
            m.attachments.push(attachment_json(
                &id,
                &filename,
                content_type,
                size as u64,
                &channel_id,
            ));
        }
    }
    Ok(())
}

/// Remove stored files for a message (before the message row is deleted).
pub async fn remove_for_message(state: &AppState, message_id: &str) -> Result<(), sqlx::Error> {
    let paths: Vec<(String, String)> = sqlx::query_as(&crate::db::sql(
        "SELECT path, channel_id FROM attachments WHERE message_id = ?",
    ))
    .bind(message_id)
    .fetch_all(&state.db)
    .await?;
    for (rel, _) in paths {
        let full = data_dir().join(&rel);
        let _ = tokio::fs::remove_file(&full).await;
        if let Some(parent) = full.parent() {
            let _ = tokio::fs::remove_dir(parent).await;
        }
    }
    Ok(())
}

/// Removes stored files for every attachment in these channels, before the rows cascade away.
pub async fn remove_for_channels(
    state: &AppState,
    channel_ids: &[String],
) -> Result<(), sqlx::Error> {
    for channel_id in channel_ids {
        let paths: Vec<(String,)> = sqlx::query_as(&crate::db::sql(
            "SELECT path FROM attachments WHERE channel_id = ?",
        ))
        .bind(channel_id)
        .fetch_all(&state.db)
        .await?;
        for (rel,) in paths {
            let full = data_dir().join(&rel);
            let _ = tokio::fs::remove_file(&full).await;
            if let Some(parent) = full.parent() {
                let _ = tokio::fs::remove_dir(parent).await;
            }
        }
    }
    Ok(())
}

async fn serve_attachment(
    State(state): State<AppState>,
    Path((channel_id, attachment_id, filename)): Path<(String, String, String)>,
) -> Response {
    let row: Option<(String, Option<String>, String)> = match sqlx::query_as(&crate::db::sql(
        "SELECT path, content_type, filename FROM attachments WHERE id = ? AND channel_id = ?",
    ))
    .bind(&attachment_id)
    .bind(&channel_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return crate::api::internal(e),
    };
    let Some((rel, content_type, stored_name)) = row else {
        return api_error(StatusCode::NOT_FOUND, 10014, "Unknown attachment");
    };
    if stored_name != filename {
        return api_error(StatusCode::NOT_FOUND, 10014, "Unknown attachment");
    }
    match tokio::fs::read(data_dir().join(&rel)).await {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    content_type.unwrap_or_else(|| "application/octet-stream".into()),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    format!("inline; filename=\"{stored_name}\""),
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => crate::api::internal(e),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/cdn/attachments/{channel_id}/{attachment_id}/{filename}",
            get(serve_attachment),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/attachments"),
            post(stage_uploads),
        )
        .route("/cdn/upload/{token}", put(receive_upload))
}

/// Base URL for links handed to clients: STEEL_PUBLIC_URL when set, otherwise the request's
/// scheme and host. Reverse proxies announce the public scheme and host through the
/// `X-Forwarded-Proto` and `X-Forwarded-Host` headers, which are honored first.
fn base_for(headers: &HeaderMap) -> String {
    let configured = public_base();
    if !configured.is_empty() {
        return configured;
    }
    let first = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let scheme = match first("x-forwarded-proto").as_deref() {
        Some("https") => "https",
        _ => "http",
    };
    let host = first("x-forwarded-host")
        .or_else(|| first(header::HOST.as_str()))
        .unwrap_or_else(|| "localhost".to_string());
    format!("{scheme}://{host}")
}

/// `POST /channels/{id}/attachments`: reserve upload slots for files about to be sent.
async fn stage_uploads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(req): Json<StageFiles>,
) -> Response {
    let result: Result<Response, Response> = async {
        let user = crate::chat::authed(&state, &headers).await?;
        let channel = crate::chat::channel_row(&state, &channel_id).await?;
        crate::chat::require_channel(&state, &channel, &user.id).await?;
        if req.files.is_empty() || req.files.len() > MAX_FILES {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, format!("send 1-{MAX_FILES} files per request")));
        }
        let max = max_file_bytes();
        let base = base_for(&headers);
        let mut out = Vec::with_capacity(req.files.len());
        for file in req.files {
            if file.file_size > max {
                return Err(api_error(StatusCode::PAYLOAD_TOO_LARGE, 40005, "file is too large"));
            }
            let token = auth::new_token();
            sqlx::query(&crate::db::sql("INSERT INTO pending_uploads (token, channel_id, user_id, filename, content_type, size, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)"))
                .bind(&token)
                .bind(&channel_id)
                .bind(&user.id)
                .bind(sanitize_filename(&file.filename))
                .bind(file.original_content_type.clone())
                .bind(file.file_size as i64)
                .bind(chrono::Utc::now().to_rfc3339())
                .execute(&state.db)
                .await
                .map_err(crate::api::internal)?;
            out.push(flint_protocol::StagedUpload {
                id: file.id,
                upload_url: format!("{base}/cdn/upload/{token}"),
                upload_filename: token,
            });
        }
        Ok(Json(flint_protocol::StagedUploads { attachments: out }).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PUT /cdn/upload/{token}`: the file bytes. The token is the only credential, as with a signed URL.
async fn receive_upload(
    State(state): State<AppState>,
    Path(token): Path<String>,
    req: axum::extract::Request,
) -> Response {
    let row: Option<(String, String, i64, i64)> = match sqlx::query_as(&crate::db::sql(
        "SELECT channel_id, filename, size, uploaded FROM pending_uploads WHERE token = ?",
    ))
    .bind(&token)
    .fetch_optional(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return crate::api::internal(e),
    };
    let Some((channel_id, filename, size, uploaded)) = row else {
        return api_error(StatusCode::NOT_FOUND, 10014, "Unknown upload");
    };
    if uploaded != 0 {
        return api_error(
            StatusCode::CONFLICT,
            10014,
            "This upload was already received",
        );
    }
    let mismatch = || {
        api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "upload size does not match the staged size",
        )
    };
    let bytes = match axum::body::to_bytes(req.into_body(), size as usize + 1).await {
        Ok(b) if b.len() as i64 == size => b,
        _ => return mismatch(),
    };
    let rel = format!("attachments/{channel_id}/{token}/{filename}");
    let full = data_dir().join(&rel);
    if let Some(parent) = full.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return crate::api::internal(e);
        }
    }
    if let Err(e) = tokio::fs::write(&full, &bytes).await {
        return crate::api::internal(e);
    }
    if let Err(e) = sqlx::query(&crate::db::sql(
        "UPDATE pending_uploads SET path = ?, uploaded = 1 WHERE token = ?",
    ))
    .bind(&rel)
    .bind(&token)
    .execute(&state.db)
    .await
    {
        return crate::api::internal(e);
    }
    StatusCode::OK.into_response()
}

/// A staged upload that a message is claiming.
pub struct Claimed {
    pub token: String,
    pub path: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size: i64,
}

/// Check that every reference is a finished upload by this user in this channel.
pub async fn claim(
    state: &AppState,
    channel_id: &str,
    user_id: &str,
    tokens: &[String],
) -> Result<Vec<Claimed>, Response> {
    let bad = || {
        api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "unknown or unfinished attachment",
        )
    };
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        let row: Option<(Option<String>, String, Option<String>, i64)> =
            sqlx::query_as(&crate::db::sql(
                "SELECT path, filename, content_type, size FROM pending_uploads \
             WHERE token = ? AND channel_id = ? AND user_id = ? AND uploaded = 1",
            ))
            .bind(token)
            .bind(channel_id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(crate::api::internal)?;
        let Some((Some(path), filename, content_type, size)) = row else {
            return Err(bad());
        };
        out.push(Claimed {
            token: token.clone(),
            path,
            filename,
            content_type,
            size,
        });
    }
    Ok(out)
}

/// Move claimed uploads onto a message and drop their pending rows.
pub async fn attach_claimed(
    state: &AppState,
    channel_id: &str,
    message_id: &str,
    claimed: &[Claimed],
) -> Result<(), sqlx::Error> {
    let mut tx = state.db.begin().await?;
    for c in claimed {
        sqlx::query(&crate::db::sql("INSERT INTO attachments (id, message_id, channel_id, filename, content_type, size, path) VALUES (?, ?, ?, ?, ?, ?, ?)"))
            .bind(auth::new_random_numeric_id())
            .bind(message_id)
            .bind(channel_id)
            .bind(&c.filename)
            .bind(&c.content_type)
            .bind(c.size)
            .bind(&c.path)
            .execute(&mut *tx)
            .await?;
        sqlx::query(&crate::db::sql(
            "DELETE FROM pending_uploads WHERE token = ?",
        ))
        .bind(&c.token)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

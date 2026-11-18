//! File uploads and the CDN. Attachments are stored under `STEEL_DATA_DIR/attachments/`.
//! Attachment IDs are random tokens, so the CDN URL is not guessable. The URL itself does not require a
//! session, because image and file widgets fetch it without headers.

use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use flint_protocol::Attachment;

use crate::{api::api_error, auth, state::AppState};

/// Per-file limit. Override with STEEL_MAX_UPLOAD_BYTES.
pub fn max_file_bytes() -> u64 {
    std::env::var("STEEL_MAX_UPLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8 * 1024 * 1024)
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
    let id = auth::new_token();
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
    sqlx::query("INSERT INTO attachments (id, message_id, channel_id, filename, content_type, size, path) VALUES (?, ?, ?, ?, ?, ?, ?)")
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
    let sql = format!(
        "SELECT message_id, id, channel_id, filename, content_type, size FROM attachments WHERE message_id IN ({placeholders}) ORDER BY rowid"
    );
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
    let paths: Vec<(String, String)> =
        sqlx::query_as("SELECT path, channel_id FROM attachments WHERE message_id = ?")
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

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/cdn/attachments/{channel_id}/{attachment_id}/{filename}",
        get(serve_attachment),
    )
}

async fn serve_attachment(
    State(state): State<AppState>,
    Path((channel_id, attachment_id, filename)): Path<(String, String, String)>,
) -> Response {
    let row: Option<(String, Option<String>, String)> = match sqlx::query_as(
        "SELECT path, content_type, filename FROM attachments WHERE id = ? AND channel_id = ?",
    )
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

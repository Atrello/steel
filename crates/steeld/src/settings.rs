//! Account settings as opaque protobuf blobs, one per kind. The client owns the schema and encoding.
//! Steel keeps the bytes, checks writes against the data version (field 1, a varint the client sets),
//! and pushes changes to the user's sessions with `USER_SETTINGS_PROTO_UPDATE`.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::{api_error, API_PREFIX},
    chat::{self, ApiResult},
    state::AppState,
};

/// Settings kinds Flint uses: 1 is preloaded user settings, 2 holds GIF favorites.
const KINDS: [u8; 2] = [1, 2];
/// The blob a new account starts with: field 1 (data version) set to 0.
const EMPTY_PROTO: &str = "CAA=";
const MAX_PROTO_BYTES: usize = 64 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{API_PREFIX}/users/@me/settings-proto/{{kind}}"),
        get(read).patch(write),
    )
}

#[derive(Deserialize)]
struct WriteBody {
    settings: String,
    required_data_version: u64,
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

/// The data version in a settings blob: the varint in field 1, or 0 if absent.
pub fn data_version(proto: &[u8]) -> Option<u64> {
    let mut i = 0;
    let read_varint = |i: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *proto.get(*i)?;
            *i += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    };
    while i < proto.len() {
        let tag = read_varint(&mut i)?;
        let (field, wire) = (tag >> 3, tag & 7);
        if field == 1 && wire == 0 {
            return read_varint(&mut i);
        }
        match wire {
            0 => {
                read_varint(&mut i)?;
            }
            1 => i = i.checked_add(8)?,
            2 => {
                let len = read_varint(&mut i)? as usize;
                i = i.checked_add(len)?;
            }
            5 => i = i.checked_add(4)?,
            _ => return None,
        }
        if i > proto.len() {
            return None;
        }
    }
    Some(0)
}

async fn stored(state: &AppState, user_id: &str, kind: u8) -> Result<String, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(&crate::db::sql(
        "SELECT proto FROM user_settings WHERE user_id = ? AND kind = ?",
    ))
    .bind(user_id)
    .bind(i64::from(kind))
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(p,)| p).unwrap_or_else(|| EMPTY_PROTO.to_string()))
}

fn version_of_b64(proto: &str) -> Option<u64> {
    let bytes = STANDARD.decode(proto).ok()?;
    data_version(&bytes)
}

fn check_kind(kind: u8) -> ApiResult<()> {
    if KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(api_error(StatusCode::NOT_FOUND, 0, "Unknown settings kind"))
    }
}

async fn read(State(state): State<AppState>, headers: HeaderMap, Path(kind): Path<u8>) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        check_kind(kind)?;
        let proto = stored(&state, &user.id, kind).await.map_err(db)?;
        Ok(Json(json!({ "settings": proto })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn write(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(kind): Path<u8>,
    Json(body): Json<WriteBody>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        check_kind(kind)?;
        let current = stored(&state, &user.id, kind).await.map_err(db)?;
        let current_version = version_of_b64(&current).unwrap_or(0);
        if body.required_data_version != current_version {
            // The client's copy is stale: send the current blob back with out_of_date, as Discord does.
            return Ok((StatusCode::CONFLICT, Json(json!({ "settings": current, "out_of_date": true }))).into_response());
        }
        let bytes = STANDARD
            .decode(&body.settings)
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, 50035, "settings must be base64"))?;
        if bytes.is_empty() || bytes.len() > MAX_PROTO_BYTES {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "settings are out of bounds"));
        }
        let new_version = data_version(&bytes)
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, 50035, "settings are not valid protobuf"))?;
        if new_version <= current_version {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "the data version must increase"));
        }
        sqlx::query(&crate::db::sql(
            "INSERT INTO user_settings (user_id, kind, proto, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (user_id, kind) DO UPDATE SET proto = excluded.proto, updated_at = excluded.updated_at",
        ))
        .bind(&user.id)
        .bind(i64::from(kind))
        .bind(&body.settings)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        state.publish(
            vec![user.id.clone()],
            "USER_SETTINGS_PROTO_UPDATE",
            json!({ "settings": { "type": kind, "proto": body.settings } }),
        );
        Ok(Json(json!({ "settings": body.settings })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

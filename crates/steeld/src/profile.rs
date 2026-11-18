//! `GET /users/{id}/profile`: the profile popout. `PATCH /users/@me` edits the display name, bio,
//! pronouns and accent color. Banners, connections and badges are not stored yet, so they come back empty.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    api::API_PREFIX,
    chat::{self, ApiResult},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{API_PREFIX}/users/{{user_id}}/profile"),
        get(get_profile),
    )
}

/// global_name, bio, pronouns, accent_color.
type ProfileRow = (Option<String>, Option<String>, Option<String>, Option<i64>);

const MAX_GLOBAL_NAME: usize = 32;
const MAX_BIO: usize = 190;
const MAX_PRONOUNS: usize = 40;

/// Distinguishes an absent field (outer `None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// Body of `PATCH /users/@me`. Absent fields are left alone; `null` clears a field.
#[derive(Deserialize)]
pub struct EditMe {
    #[serde(default, deserialize_with = "double_option")]
    global_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    bio: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pronouns: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    accent_color: Option<Option<u32>>,
}

fn invalid(message: &str) -> Response {
    crate::api::api_error(StatusCode::BAD_REQUEST, 50035, message)
}

fn bounded(value: Option<String>, max: usize) -> Result<Option<String>, &'static str> {
    match value {
        None => Ok(None),
        Some(text) => {
            let text = text.trim().to_string();
            if text.chars().count() > max {
                Err("profile text is too long")
            } else {
                Ok(Some(text).filter(|t| !t.is_empty()))
            }
        }
    }
}

/// `PATCH /users/@me`: edits the caller's profile and returns the account user object.
pub async fn edit_me(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EditMe>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let mut name_change: Option<Option<String>> = None;
        if let Some(value) = body.global_name {
            name_change = Some(bounded(value, MAX_GLOBAL_NAME).map_err(invalid_text)?);
        }
        let mut bio_change: Option<Option<String>> = None;
        if let Some(value) = body.bio {
            bio_change = Some(bounded(value, MAX_BIO).map_err(invalid_text)?);
        }
        let mut pronouns_change: Option<Option<String>> = None;
        if let Some(value) = body.pronouns {
            pronouns_change = Some(bounded(value, MAX_PRONOUNS).map_err(invalid_text)?);
        }
        let mut accent_change: Option<Option<u32>> = None;
        if let Some(value) = body.accent_color {
            if let Some(color) = value {
                if color > 0xFF_FFFF {
                    return Err(invalid("accent_color must be a 24-bit RGB value"));
                }
            }
            accent_change = Some(value);
        }

        // Read the current row so untouched fields keep their values.
        let current: Option<ProfileRow> = sqlx::query_as(&crate::db::sql(
            "SELECT global_name, bio, pronouns, accent_color FROM user_profiles WHERE user_id = ?",
        ))
        .bind(&user.id)
        .fetch_optional(&state.db)
        .await
        .map_err(chat::db)?;
        let (mut g, mut b, mut p, mut a) = current.unwrap_or((None, None, None, None));
        if let Some(v) = name_change { g = v; }
        if let Some(v) = bio_change { b = v; }
        if let Some(v) = pronouns_change { p = v; }
        if let Some(v) = accent_change { a = v.map(i64::from); }
        sqlx::query(&crate::db::sql(
            "INSERT INTO user_profiles (user_id, global_name, bio, pronouns, accent_color, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (user_id) DO UPDATE SET global_name = excluded.global_name, bio = excluded.bio, \
             pronouns = excluded.pronouns, accent_color = excluded.accent_color, updated_at = excluded.updated_at",
        ))
        .bind(&user.id)
        .bind(&g)
        .bind(&b)
        .bind(&p)
        .bind(a)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(chat::db)?;

        let account = json!({ "id": user.id, "username": user.username, "global_name": g, "avatar": null, "bot": false });
        state.publish(vec![user.id.clone()], "USER_UPDATE", account.clone());
        Ok(Json(account).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

fn invalid_text(message: &str) -> Response {
    invalid(message)
}

async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        chat::authed(&state, &headers).await?;
        let (id, username): (String, String) = sqlx::query_as(&crate::db::sql(
            "SELECT id, username FROM users WHERE id = ?",
        ))
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(chat::db)?
        .ok_or_else(chat::missing_member)?;
        let profile: Option<ProfileRow> = sqlx::query_as(&crate::db::sql(
            "SELECT global_name, bio, pronouns, accent_color FROM user_profiles WHERE user_id = ?",
        ))
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(chat::db)?;
        let (global_name, bio, pronouns, accent) = profile.unwrap_or((None, None, None, None));
        let body: Value = json!({
            "user": {
                "id": id,
                "username": username,
                "global_name": global_name,
                "avatar": null,
                "bot": false,
                "discriminator": "0",
            },
            "user_profile": {
                "bio": bio,
                "pronouns": pronouns,
                "banner": null,
                "accent_color": accent,
                "theme_colors": null,
            },
            "guild_member": null,
            "guild_member_profile": null,
            "badges": [],
            "guild_badges": [],
            "connected_accounts": [],
            "mutual_guilds": [],
            "mutual_friends": [],
            "private": false,
        });
        Ok((StatusCode::OK, Json(body)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

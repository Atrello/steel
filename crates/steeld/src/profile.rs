//! `GET /users/{id}/profile`: the profile popout. Steel stores no bio, pronouns, banner or
//! connections yet, so those come back empty; the shape matches what Flint decodes.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
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
        let body: Value = json!({
            "user": {
                "id": id,
                "username": username,
                "global_name": null,
                "avatar": null,
                "bot": false,
                "discriminator": "0",
            },
            "user_profile": {
                "bio": null,
                "pronouns": null,
                "banner": null,
                "accent_color": null,
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

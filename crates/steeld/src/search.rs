//! Message search within a guild or a DM. Matches message text (case-insensitive substring), newest first.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use flint_protocol::Message;
use serde::{Deserialize, Serialize};

use crate::{
    api::API_PREFIX,
    chat::{self, ApiResult, MESSAGE_SELECT},
    state::AppState,
};

const PAGE: u32 = 25;
/// The client caps search offsets; beyond this it pages by `max_id` instead.
const MAX_OFFSET: u32 = 9_975;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/messages/search"),
            get(search_guild),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/messages/search"),
            get(search_channel),
        )
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub content: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Serialize)]
struct SearchPageOut {
    /// One group per hit: the matching message, with no surrounding context.
    messages: Vec<Vec<Message>>,
    total_results: u64,
}

fn like_pattern(q: &str) -> String {
    let escaped: String = q
        .chars()
        .flat_map(|c| match c {
            '\\' | '%' | '_' => vec!['\\', c],
            other => vec![other],
        })
        .collect();
    format!("%{escaped}%")
}

async fn run_search(
    state: &AppState,
    scope_sql: &str,
    scope_bind: &str,
    me: &str,
    q: SearchQuery,
) -> ApiResult<Response> {
    let term = q.content.unwrap_or_default();
    let term = term.trim();
    if term.is_empty() {
        return Ok(Json(SearchPageOut {
            messages: Vec::new(),
            total_results: 0,
        })
        .into_response());
    }
    let limit = q.limit.unwrap_or(PAGE).clamp(1, PAGE) as i64;
    let offset = q.offset.unwrap_or(0).min(MAX_OFFSET) as i64;
    let pattern = like_pattern(term);

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM messages m JOIN channels c ON c.id = m.channel_id WHERE {scope_sql} AND m.content LIKE ? ESCAPE '\\'"
    ))
    .bind(scope_bind)
    .bind(&pattern)
    .fetch_one(&state.db)
    .await
    .map_err(crate::api::internal)?;

    let rows: Vec<chat::MessageRow> = sqlx::query_as(&format!(
        "{MESSAGE_SELECT} WHERE {scope_sql} AND m.content LIKE ? ESCAPE '\\' \
         ORDER BY CAST(m.id AS INTEGER) DESC LIMIT ? OFFSET ?"
    ))
    .bind(scope_bind)
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    .map_err(crate::api::internal)?;

    let mut messages: Vec<Message> = rows.into_iter().map(chat::message_json).collect();
    crate::uploads::hydrate(state, &mut messages)
        .await
        .map_err(crate::api::internal)?;
    crate::reactions::hydrate(state, me, &mut messages)
        .await
        .map_err(crate::api::internal)?;
    Ok(Json(SearchPageOut {
        messages: messages.into_iter().map(|m| vec![m]).collect(),
        total_results: total as u64,
    })
    .into_response())
}

async fn search_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Query(q): Query<SearchQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        run_search(&state, "c.guild_id = ?", &guild_id, &user.id, q).await
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn search_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Query(q): Query<SearchQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        run_search(&state, "c.id = ?", &channel_id, &user.id, q).await
    }
    .await;
    result.unwrap_or_else(|e| e)
}

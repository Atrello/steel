//! Audit log. Steel records who did what in a guild: kicks, bans and unbans, channel create and delete,
//! and emoji create, rename and delete. Reading the log needs `VIEW_AUDIT_LOG`.

use axum::{
    extract::{Path, Query as AxumQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    api::API_PREFIX,
    auth,
    chat::{self, ApiResult},
    perms,
    state::AppState,
};

pub const KICK: u16 = 20;
pub const BAN_ADD: u16 = 22;
pub const BAN_REMOVE: u16 = 23;
pub const CHANNEL_CREATE: u16 = 10;
pub const CHANNEL_DELETE: u16 = 12;
pub const EMOJI_CREATE: u16 = 60;
pub const EMOJI_UPDATE: u16 = 61;
pub const EMOJI_DELETE: u16 = 62;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{API_PREFIX}/guilds/{{guild_id}}/audit-logs"),
        get(list),
    )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

/// Writes one entry. `changes` is a list of `{key, old_value, new_value}` objects.
pub async fn record(
    state: &AppState,
    guild_id: &str,
    actor_id: &str,
    action: u16,
    target_id: Option<&str>,
    changes: Value,
    options: Option<Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(&crate::db::sql(
        "INSERT INTO audit_log (id, guild_id, user_id, target_id, action_type, reason, changes, options, created_at) \
         VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?)",
    ))
    .bind(auth::new_id())
    .bind(guild_id)
    .bind(actor_id)
    .bind(target_id)
    .bind(i64::from(action))
    .bind(changes.to_string())
    .bind(options.map(|o| o.to_string()))
    .bind(chat::now_iso())
    .execute(&state.db)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    action_type: Option<i64>,
}

type Row = (
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    String,
    Option<String>,
);

/// `GET /guilds/{id}/audit-logs`: newest first. `before` is an entry id to page back from.
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    AxumQuery(query): AxumQuery<ListQuery>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::VIEW_AUDIT_LOG).await?;
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

        let mut sql = String::from(
            "SELECT id, user_id, target_id, action_type, reason, changes, options FROM audit_log WHERE guild_id = ?",
        );
        if query.user_id.is_some() {
            sql.push_str(" AND user_id = ?");
        }
        if query.action_type.is_some() {
            sql.push_str(" AND action_type = ?");
        }
        if query.before.is_some() {
            sql.push_str(" AND CAST(id AS BIGINT) < CAST(? AS BIGINT)");
        }
        sql.push_str(" ORDER BY CAST(id AS BIGINT) DESC LIMIT ?");

        let statement = crate::db::sql(&sql);
        let mut q = sqlx::query_as::<_, Row>(&statement).bind(&guild_id);
        if let Some(u) = &query.user_id {
            q = q.bind(u);
        }
        if let Some(a) = query.action_type {
            q = q.bind(a);
        }
        if let Some(b) = &query.before {
            q = q.bind(b);
        }
        let rows = q.bind(limit).fetch_all(&state.db).await.map_err(db)?;

        let mut entries = Vec::with_capacity(rows.len());
        let mut user_ids: Vec<String> = Vec::new();
        for (id, actor, target, action, reason, changes, options) in rows {
            if let Some(a) = &actor {
                if !user_ids.contains(a) {
                    user_ids.push(a.clone());
                }
            }
            let mut entry = json!({
                "id": id,
                "user_id": actor,
                "target_id": target,
                "action_type": action,
                "reason": reason,
                "changes": serde_json::from_str::<Value>(&changes).unwrap_or(json!([])),
            });
            if let Some(options) = options.and_then(|o| serde_json::from_str::<Value>(&o).ok()) {
                entry["options"] = options;
            }
            entries.push(entry);
        }
        let mut users = Vec::new();
        for id in user_ids {
            let name: Option<(String,)> = sqlx::query_as(&crate::db::sql("SELECT username FROM users WHERE id = ?"))
                .bind(&id)
                .fetch_optional(&state.db)
                .await
                .map_err(db)?;
            if let Some((username,)) = name {
                users.push(serde_json::to_value(chat::plain_user(id, username)).unwrap_or_default());
            }
        }
        Ok((StatusCode::OK, Json(json!({ "audit_log_entries": entries, "users": users }))).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

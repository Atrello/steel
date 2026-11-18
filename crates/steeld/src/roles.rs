//! Roles: CRUD, positions, member counts, member role assignment, and the GUILD_ROLE_* events.
//! Every role lives in its guild. The `@everyone` role has the guild's id and cannot be deleted.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    api::{api_error, API_PREFIX},
    auth,
    chat::{self, ApiResult},
    perms::{self, MANAGE_ROLES},
    state::AppState,
};

const MAX_ROLES: i64 = 250;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/roles"),
            get(list_roles).post(create_role).patch(reorder_roles),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/roles/member-counts"),
            get(member_counts),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/roles/{{role_id}}"),
            axum::routing::patch(edit_role).delete(delete_role),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/members/{{user_id}}/roles/{{role_id}}"),
            put(add_member_role).delete(remove_member_role),
        )
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: String,
    name: String,
    permissions: String,
    color: i64,
    hoist: i64,
    mentionable: i64,
    position: i64,
}

fn role_json(row: &RoleRow) -> Value {
    json!({
        "id": row.id,
        "name": row.name,
        "permissions": row.permissions,
        "color": row.color,
        "colors": {
            "primary_color": row.color,
            "secondary_color": null,
            "tertiary_color": null,
        },
        "position": row.position,
        "hoist": row.hoist != 0,
        "mentionable": row.mentionable != 0,
        "managed": false,
        "icon": null,
        "unicode_emoji": null,
    })
}

/// All roles of a guild, lowest position first, ties broken by id.
pub async fn roles_for_guild(state: &AppState, guild_id: &str) -> ApiResult<Vec<Value>> {
    roles_for_guild_db(state, guild_id).await.map_err(chat::db)
}

/// Same as `roles_for_guild`, with the database error left for the caller to map.
pub async fn roles_for_guild_db(
    state: &AppState,
    guild_id: &str,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<RoleRow> = sqlx::query_as(&crate::db::sql(
        "SELECT id, name, permissions, color, hoist, mentionable, position FROM roles \
         WHERE guild_id = ? ORDER BY position, CAST(id AS BIGINT)",
    ))
    .bind(guild_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(role_json).collect())
}

async fn role_row(state: &AppState, guild_id: &str, role_id: &str) -> ApiResult<RoleRow> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, name, permissions, color, hoist, mentionable, position FROM roles \
         WHERE id = ? AND guild_id = ?",
    ))
    .bind(role_id)
    .bind(guild_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10011, "Unknown Role"))
}

async fn guild_members(state: &AppState, guild_id: &str) -> ApiResult<Vec<String>> {
    chat::member_ids(state, guild_id).await.map_err(chat::db)
}

/// Permission bits from a JSON string or number. Unknown high bits are kept as sent.
fn parse_bits(value: &Value) -> ApiResult<u64> {
    match value {
        Value::String(s) => s
            .parse::<u64>()
            .map_err(|_| bad("permissions must be a number")),
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| bad("permissions must be a number")),
        _ => Err(bad("permissions must be a number")),
    }
}

fn bad(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

/// Color from `color` or `colors.primary_color`.
fn parse_color(fields: &serde_json::Map<String, Value>) -> ApiResult<Option<i64>> {
    if let Some(value) = fields.get("color") {
        return match value {
            Value::Null => Ok(Some(0)),
            Value::Number(n) => n
                .as_i64()
                .filter(|c| (0..=0xFF_FFFF).contains(c))
                .map(Some)
                .ok_or_else(|| bad("color out of range")),
            _ => Err(bad("color must be a number")),
        };
    }
    if let Some(colors) = fields.get("colors") {
        let primary = colors.get("primary_color").cloned().unwrap_or(Value::Null);
        return match primary {
            Value::Null => Ok(Some(0)),
            Value::Number(n) => n
                .as_i64()
                .filter(|c| (0..=0xFF_FFFF).contains(c))
                .map(Some)
                .ok_or_else(|| bad("color out of range")),
            _ => Err(bad("color must be a number")),
        };
    }
    Ok(None)
}

async fn publish_role(state: &AppState, guild_id: &str, event: &'static str, role: Value) {
    let targets = guild_members(state, guild_id).await.unwrap_or_default();
    state.publish(
        targets,
        event,
        json!({ "guild_id": guild_id, "role": role }),
    );
}

async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        Ok(Json(roles_for_guild(&state, &guild_id).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
        let fields = body.as_object().ok_or_else(|| bad("body must be an object"))?;
        let name = match fields.get("name") {
            Some(Value::String(s)) => chat::check_name(s)?,
            None => "new role".to_string(),
            Some(_) => return Err(bad("name must be a string")),
        };
        let permissions = match fields.get("permissions") {
            Some(v) => parse_bits(v)?,
            None => 0,
        };
        let color = parse_color(fields)?.unwrap_or(0);
        let hoist = fields.get("hoist").and_then(Value::as_bool).unwrap_or(false);
        let mentionable = fields.get("mentionable").and_then(Value::as_bool).unwrap_or(false);
        let count: i64 = sqlx::query_scalar(&crate::db::sql("SELECT COUNT(*) FROM roles WHERE guild_id = ?"))
            .bind(&guild_id)
            .fetch_one(&state.db)
            .await
            .map_err(chat::db)?;
        if count >= MAX_ROLES {
            return Err(bad("too many roles in this guild"));
        }
        let position: i64 = sqlx::query_scalar(&crate::db::sql("SELECT COALESCE(MAX(position), 0) + 1 FROM roles WHERE guild_id = ?"))
            .bind(&guild_id)
            .fetch_one(&state.db)
            .await
            .map_err(chat::db)?;
        let position = if crate::perms::is_owner(&state, &guild_id, &user.id).await? {
            position
        } else {
            1
        };
        let role_id = auth::new_id();
        sqlx::query(&crate::db::sql("INSERT INTO roles (id, guild_id, name, permissions, color, hoist, mentionable, position, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"))
            .bind(&role_id)
            .bind(&guild_id)
            .bind(&name)
            .bind(permissions.to_string())
            .bind(color)
            .bind(hoist as i64)
            .bind(mentionable as i64)
            .bind(position)
            .bind(chat::now_iso())
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        let role = role_json(&role_row(&state, &guild_id, &role_id).await?);
        publish_role(&state, &guild_id, "GUILD_ROLE_CREATE", role.clone()).await;
        Ok((StatusCode::OK, Json(role)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn edit_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
        if role_id != guild_id {
            crate::perms::require_above_role(&state, &guild_id, &user.id, &role_id).await?;
        }
        let current = role_row(&state, &guild_id, &role_id).await?;
        let fields = body.as_object().ok_or_else(|| bad("body must be an object"))?;
        let is_everyone = role_id == guild_id;
        let name = match fields.get("name") {
            None => current.name.clone(),
            Some(Value::String(s)) => {
                if is_everyone {
                    return Err(bad("the @everyone role cannot be renamed"));
                }
                chat::check_name(s)?
            }
            Some(_) => return Err(bad("name must be a string")),
        };
        let permissions = match fields.get("permissions") {
            None => current.permissions.clone(),
            Some(v) => parse_bits(v)?.to_string(),
        };
        let color = parse_color(fields)?.unwrap_or(current.color);
        let hoist = fields.get("hoist").and_then(Value::as_bool).map_or(current.hoist != 0, |v| v);
        let mentionable = fields
            .get("mentionable")
            .and_then(Value::as_bool)
            .map_or(current.mentionable != 0, |v| v);
        sqlx::query(&crate::db::sql("UPDATE roles SET name = ?, permissions = ?, color = ?, hoist = ?, mentionable = ? WHERE id = ?"))
            .bind(&name)
            .bind(&permissions)
            .bind(color)
            .bind(hoist as i64)
            .bind(mentionable as i64)
            .bind(&role_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        let role = role_json(&role_row(&state, &guild_id, &role_id).await?);
        publish_role(&state, &guild_id, "GUILD_ROLE_UPDATE", role.clone()).await;
        Ok(Json(role).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn delete_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, role_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
        if role_id == guild_id {
            return Err(bad("the @everyone role cannot be deleted"));
        }
        role_row(&state, &guild_id, &role_id).await?;
        crate::perms::require_above_role(&state, &guild_id, &user.id, &role_id).await?;
        let targets = guild_members(&state, &guild_id).await?;
        let mut tx = state.db.begin().await.map_err(chat::db)?;
        sqlx::query(&crate::db::sql(
            "DELETE FROM overwrites WHERE target_id = ? AND target_type = ?",
        ))
        .bind(&role_id)
        .bind(perms::TARGET_ROLE as i64)
        .execute(&mut *tx)
        .await
        .map_err(chat::db)?;
        sqlx::query(&crate::db::sql(
            "DELETE FROM roles WHERE id = ? AND guild_id = ?",
        ))
        .bind(&role_id)
        .bind(&guild_id)
        .execute(&mut *tx)
        .await
        .map_err(chat::db)?;
        tx.commit().await.map_err(chat::db)?;
        state.publish(
            targets,
            "GUILD_ROLE_DELETE",
            json!({ "guild_id": guild_id, "role_id": role_id }),
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PATCH /guilds/{id}/roles`: `[{id, position}]`. Returns every role in its new order.
async fn reorder_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(entries): Json<Vec<Value>>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
        let top = crate::perms::top_position(&state, &guild_id, &user.id).await?;
        let mut tx = state.db.begin().await.map_err(chat::db)?;
        for entry in &entries {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("each entry needs an id"))?;
            let position = entry
                .get("position")
                .and_then(Value::as_i64)
                .ok_or_else(|| bad("each entry needs a position"))?;
            if id == guild_id {
                return Err(bad("the @everyone role stays at the bottom"));
            }
            crate::perms::require_above_role(&state, &guild_id, &user.id, id).await?;
            if top != i64::MAX && position >= top {
                return Err(crate::perms::missing());
            }
            sqlx::query(&crate::db::sql(
                "UPDATE roles SET position = ? WHERE id = ? AND guild_id = ?",
            ))
            .bind(position.max(1))
            .bind(id)
            .bind(&guild_id)
            .execute(&mut *tx)
            .await
            .map_err(chat::db)?;
        }
        tx.commit().await.map_err(chat::db)?;
        let roles = roles_for_guild(&state, &guild_id).await?;
        let targets = guild_members(&state, &guild_id).await?;
        for role in &roles {
            state.publish(
                targets.clone(),
                "GUILD_ROLE_UPDATE",
                json!({ "guild_id": guild_id, "role": role }),
            );
        }
        Ok(Json(roles).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn member_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let rows: Vec<(String, i64)> = sqlx::query_as(&crate::db::sql(
            "SELECT role_id, COUNT(*) FROM member_roles WHERE guild_id = ? GROUP BY role_id",
        ))
        .bind(&guild_id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?;
        let mut counts = serde_json::Map::new();
        let members: i64 = sqlx::query_scalar(&crate::db::sql(
            "SELECT COUNT(*) FROM guild_members WHERE guild_id = ?",
        ))
        .bind(&guild_id)
        .fetch_one(&state.db)
        .await
        .map_err(chat::db)?;
        counts.insert(guild_id.clone(), json!(members));
        for (role, count) in rows {
            counts.insert(role, json!(count));
        }
        Ok(Json(Value::Object(counts)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Sends `GUILD_MEMBER_UPDATE` with the member's current roles to the guild.
pub async fn publish_member_roles(
    state: &AppState,
    guild_id: &str,
    user_id: &str,
) -> ApiResult<()> {
    let (username,): (String,) =
        sqlx::query_as(&crate::db::sql("SELECT username FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_one(&state.db)
            .await
            .map_err(chat::db)?;
    let nick: Option<String> = sqlx::query_scalar(&crate::db::sql(
        "SELECT nick FROM guild_members WHERE guild_id = ? AND user_id = ?",
    ))
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .flatten();
    let roles = member_role_ids(state, guild_id, user_id).await?;
    let targets = guild_members(state, guild_id).await?;
    state.publish(
        targets,
        "GUILD_MEMBER_UPDATE",
        json!({
            "guild_id": guild_id,
            "user": chat::plain_user(user_id.to_string(), username),
            "roles": roles,
            "nick": nick,
        }),
    );
    Ok(())
}

/// Role ids a member holds, excluding the implicit `@everyone`.
pub async fn member_role_ids(
    state: &AppState,
    guild_id: &str,
    user_id: &str,
) -> ApiResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        &crate::db::sql("SELECT role_id FROM member_roles WHERE guild_id = ? AND user_id = ? ORDER BY CAST(role_id AS BIGINT)"),
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(chat::db)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn add_member_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id, role_id)): Path<(String, String, String)>,
) -> Response {
    change_member_role(state, headers, guild_id, user_id, role_id, true).await
}

async fn remove_member_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((guild_id, user_id, role_id)): Path<(String, String, String)>,
) -> Response {
    change_member_role(state, headers, guild_id, user_id, role_id, false).await
}

async fn change_member_role(
    state: AppState,
    headers: HeaderMap,
    guild_id: String,
    user_id: String,
    role_id: String,
    assign: bool,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, MANAGE_ROLES).await?;
        if role_id == guild_id {
            return Err(bad("@everyone is held by every member"));
        }
        role_row(&state, &guild_id, &role_id).await?;
        crate::perms::require_above_role(&state, &guild_id, &user.id, &role_id).await?;
        chat::require_member(&state, &guild_id, &user_id).await?;
        crate::perms::require_above_member(&state, &guild_id, &user.id, &user_id).await?;
        if assign {
            sqlx::query(&crate::db::sql(
                "INSERT OR IGNORE INTO member_roles (guild_id, user_id, role_id) VALUES (?, ?, ?)",
            ))
            .bind(&guild_id)
            .bind(&user_id)
            .bind(&role_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        } else {
            sqlx::query(&crate::db::sql(
                "DELETE FROM member_roles WHERE guild_id = ? AND user_id = ? AND role_id = ?",
            ))
            .bind(&guild_id)
            .bind(&user_id)
            .bind(&role_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        }
        publish_member_roles(&state, &guild_id, &user_id).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

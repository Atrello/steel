//! Phase 4: bot accounts, application commands, interactions and followups.
//!
//! A bot is a user owned by an application. It authenticates with `Authorization: Bot <token>`
//! (or the gateway Identify token). When a member runs a command or presses a component, Steel
//! sends the bot an `INTERACTION_CREATE` with a token. The bot answers over REST with a callback
//! (`POST /interactions/{id}/{token}/callback`) or with followups, all valid for 15 minutes.
//!
//! Permissions follow Phase 1: the application's owner manages it, the guild owner adds its bot
//! to a guild, and any member of that guild can run its commands. Ephemeral messages (flag 64)
//! are delivered only to the user who ran the interaction.

use std::collections::HashSet;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use flint_protocol::{CreateApplication, Message, User};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Any, QueryBuilder};

use crate::{
    api::{api_error, internal, API_PREFIX},
    auth,
    chat::{self, ApiResult, ChannelRow},
    state::AppState,
};

/// Ephemeral message flag. Such messages reach only the invoking user.
pub const FLAG_EPHEMERAL: u32 = 64;
/// Interaction tokens (callbacks and followups) expire after this long.
const TOKEN_TTL_SECS: i64 = 15 * 60;
const MAX_COMMANDS_PER_SCOPE: i64 = 100;
const MAX_OPTIONS: usize = 25;
const MAX_COMPONENT_ROWS: usize = 5;
const MAX_COMPONENT_BYTES: usize = 16 * 1024;
const MAX_CONTENT: usize = 2000;
const MAX_DESCRIPTION: usize = 400;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/applications"),
            post(create_application),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/applications"),
            get(list_applications),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}"),
            get(get_application)
                .patch(patch_application)
                .delete(delete_application),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}/bot/reset"),
            post(reset_token),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}/guilds/{{guild_id}}"),
            axum::routing::put(add_bot_to_guild).delete(remove_bot_from_guild),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}/commands"),
            get(list_global).post(create_global).put(overwrite_global),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}/commands/{{command_id}}"),
            axum::routing::patch(edit_global).delete(delete_global),
        )
        .route(
            &format!("{API_PREFIX}/applications/{{app_id}}/guilds/{{guild_id}}/commands"),
            get(list_guild)
                .post(create_guild_command)
                .put(overwrite_guild),
        )
        .route(
            &format!(
                "{API_PREFIX}/applications/{{app_id}}/guilds/{{guild_id}}/commands/{{command_id}}"
            ),
            axum::routing::patch(edit_guild_command).delete(delete_guild_command),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/installed-applications"),
            axum::routing::get(list_installed),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/applications/{{app_id}}"),
            axum::routing::put(install_app).delete(uninstall_app),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/application-command-index"),
            get(guild_index),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/application-command-index"),
            get(channel_index),
        )
        .route(
            &format!("{API_PREFIX}/interactions"),
            post(create_interaction),
        )
        .route(
            &format!("{API_PREFIX}/interactions/{{interaction_id}}/{{token}}/callback"),
            post(callback),
        )
        .route(
            &format!("{API_PREFIX}/webhooks/{{app_id}}/{{token}}"),
            post(followup),
        )
        .route(
            &format!("{API_PREFIX}/webhooks/{{app_id}}/{{token}}/messages/{{message_id}}"),
            axum::routing::patch(edit_original),
        )
}

// ---------------------------------------------------------------------------
// Rows and shared helpers
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct AppRow {
    id: String,
    owner_id: String,
    bot_user_id: String,
    name: String,
    description: String,
}

#[derive(sqlx::FromRow)]
struct IndexRow {
    id: String,
    version: String,
    application_id: String,
    guild_id: Option<String>,
    name: String,
    description: String,
    options: String,
    app_name: String,
}

#[derive(sqlx::FromRow)]
struct InteractionRow {
    id: String,
    token: String,
    application_id: String,
    kind: i64,
    channel_id: String,
    user_id: String,
    message_id: Option<String>,
    nonce: Option<String>,
    responded: i64,
    response_message_id: Option<String>,
    created_at: String,
}

fn done(result: ApiResult<Response>) -> Response {
    result.unwrap_or_else(|e| e)
}

fn bad(message: impl Into<String>) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

fn unknown(what: &str, code: u32) -> Response {
    api_error(StatusCode::NOT_FOUND, code, format!("Unknown {what}"))
}

/// Whether `user_id` is a bot account (owns an application).
pub async fn is_bot(state: &AppState, user_id: &str) -> Result<bool, sqlx::Error> {
    let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
        "SELECT 1 FROM applications WHERE bot_user_id = ?",
    ))
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(found.is_some())
}

async fn app_row(state: &AppState, app_id: &str) -> ApiResult<AppRow> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, owner_id, bot_user_id, name, description FROM applications WHERE id = ?",
    ))
    .bind(app_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .ok_or_else(|| unknown("Application", 10002))
}

/// The application's owner may manage it.
async fn owned_app(state: &AppState, headers: &HeaderMap, app_id: &str) -> ApiResult<AppRow> {
    let user = chat::authed(state, headers).await?;
    let app = app_row(state, app_id).await?;
    if app.owner_id != user.id {
        return Err(chat::no_access());
    }
    Ok(app)
}

/// The owner, or the bot itself (a bot token may register its own commands).
async fn commandable_app(state: &AppState, headers: &HeaderMap, app_id: &str) -> ApiResult<AppRow> {
    let user = chat::authed(state, headers).await?;
    let app = app_row(state, app_id).await?;
    if app.owner_id != user.id && app.bot_user_id != user.id {
        return Err(chat::no_access());
    }
    Ok(app)
}

async fn bot_user(state: &AppState, bot_user_id: &str) -> ApiResult<User> {
    let (username,): (String,) =
        sqlx::query_as(&crate::db::sql("SELECT username FROM users WHERE id = ?"))
            .bind(bot_user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(chat::db)?
            .ok_or_else(|| unknown("user", 10013))?;
    Ok(User {
        id: bot_user_id.to_string(),
        username,
        global_name: None,
        avatar: None,
        bot: true,
    })
}

async fn app_json(state: &AppState, app: &AppRow) -> ApiResult<Value> {
    let bot = bot_user(state, &app.bot_user_id).await?;
    Ok(json!({
        "id": app.id,
        "name": app.name,
        "description": app.description,
        "owner_id": app.owner_id,
        "bot": bot,
    }))
}

fn valid_name_chars(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Username stem for a bot account derived from the application name.
fn bot_username_stem(name: &str) -> String {
    let stem: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(24)
        .collect();
    if stem.is_empty() {
        "bot".into()
    } else {
        stem
    }
}

// ---------------------------------------------------------------------------
// Applications and bot accounts
// ---------------------------------------------------------------------------

/// `POST /applications`: create an application and its bot account. Returns the bot token once.
async fn create_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateApplication>,
) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let name = chat::check_name(&req.name)?;
        let description = req.description.unwrap_or_default().trim().to_string();
        if description.chars().count() > MAX_DESCRIPTION {
            return Err(bad(format!("description must be at most {MAX_DESCRIPTION} characters")));
        }
        let stem = bot_username_stem(&name);
        let app_id = auth::new_id();
        let bot_id = auth::new_id();
        let token = auth::new_token();
        let now = chat::now_iso();
        let password_hash = auth::hash_password(&auth::new_token()).map_err(internal)?;

        let mut username = None;
        for _ in 0..8 {
            let candidate = format!("{stem}_{}", &auth::new_random_numeric_id()[..6]);
            let mut tx = state.db.begin().await.map_err(chat::db)?;
            let inserted = sqlx::query(
                &crate::db::sql("INSERT INTO users (id, username, email, password_hash, created_at) VALUES (?, ?, NULL, ?, ?)"),
            )
            .bind(&bot_id)
            .bind(&candidate)
            .bind(&password_hash)
            .bind(&now)
            .execute(&mut *tx)
            .await;
            match inserted {
                Ok(_) => {}
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
                Err(e) => return Err(chat::db(e)),
            }
            sqlx::query(&crate::db::sql("INSERT INTO applications (id, owner_id, bot_user_id, name, description, created_at) VALUES (?, ?, ?, ?, ?, ?)"))
                .bind(&app_id).bind(&user.id).bind(&bot_id).bind(&name).bind(&description).bind(&now)
                .execute(&mut *tx).await.map_err(chat::db)?;
            sqlx::query(&crate::db::sql("INSERT INTO tokens (token_hash, user_id, created_at) VALUES (?, ?, ?)"))
                .bind(auth::hash_token(&token)).bind(&bot_id).bind(&now)
                .execute(&mut *tx).await.map_err(chat::db)?;
            tx.commit().await.map_err(chat::db)?;
            username = Some(candidate);
            break;
        }
        let Some(username) = username else {
            return Err(api_error(StatusCode::CONFLICT, 0, "could not pick a free bot username; try another name"));
        };
        let bot = User { id: bot_id, username, global_name: None, avatar: None, bot: true };
        Ok((
            StatusCode::CREATED,
            Json(json!({
                "id": app_id,
                "name": name,
                "description": description,
                "owner_id": user.id,
                "bot": bot,
                "token": token,
            })),
        )
            .into_response())
    }
    .await)
}

async fn list_applications(State(state): State<AppState>, headers: HeaderMap) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let rows: Vec<AppRow> = sqlx::query_as(
            &crate::db::sql("SELECT id, owner_id, bot_user_id, name, description FROM applications WHERE owner_id = ? ORDER BY CAST(id AS BIGINT)"),
        )
        .bind(&user.id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(app_json(&state, row).await?);
        }
        Ok(Json(out).into_response())
    }
    .await)
}

async fn get_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(
        async {
            let app = owned_app(&state, &headers, &app_id).await?;
            Ok(Json(app_json(&state, &app).await?).into_response())
        }
        .await,
    )
}

#[derive(Deserialize)]
struct PatchApplication {
    name: Option<String>,
    description: Option<String>,
}

async fn patch_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Json(req): Json<PatchApplication>,
) -> Response {
    done(
        async {
            let mut app = owned_app(&state, &headers, &app_id).await?;
            if let Some(name) = req.name {
                app.name = chat::check_name(&name)?;
            }
            if let Some(description) = req.description {
                let description = description.trim().to_string();
                if description.chars().count() > MAX_DESCRIPTION {
                    return Err(bad(format!(
                        "description must be at most {MAX_DESCRIPTION} characters"
                    )));
                }
                app.description = description;
            }
            sqlx::query(&crate::db::sql(
                "UPDATE applications SET name = ?, description = ? WHERE id = ?",
            ))
            .bind(&app.name)
            .bind(&app.description)
            .bind(&app.id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
            Ok(Json(app_json(&state, &app).await?).into_response())
        }
        .await,
    )
}

/// Deleting an application revokes its bot's tokens and removes it from every guild.
/// The bot's user row stays, so its old messages keep their author.
async fn delete_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(
        async {
            let app = owned_app(&state, &headers, &app_id).await?;
            let mut tx = state.db.begin().await.map_err(chat::db)?;
            sqlx::query(&crate::db::sql("DELETE FROM tokens WHERE user_id = ?"))
                .bind(&app.bot_user_id)
                .execute(&mut *tx)
                .await
                .map_err(chat::db)?;
            sqlx::query(&crate::db::sql(
                "DELETE FROM guild_members WHERE user_id = ?",
            ))
            .bind(&app.bot_user_id)
            .execute(&mut *tx)
            .await
            .map_err(chat::db)?;
            sqlx::query(&crate::db::sql("DELETE FROM applications WHERE id = ?"))
                .bind(&app.id)
                .execute(&mut *tx)
                .await
                .map_err(chat::db)?;
            tx.commit().await.map_err(chat::db)?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        .await,
    )
}

/// Issue a new bot token. The previous token stops working.
async fn reset_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(
        async {
            let app = owned_app(&state, &headers, &app_id).await?;
            let token = auth::new_token();
            let mut tx = state.db.begin().await.map_err(chat::db)?;
            sqlx::query(&crate::db::sql("DELETE FROM tokens WHERE user_id = ?"))
                .bind(&app.bot_user_id)
                .execute(&mut *tx)
                .await
                .map_err(chat::db)?;
            sqlx::query(&crate::db::sql(
                "INSERT INTO tokens (token_hash, user_id, created_at) VALUES (?, ?, ?)",
            ))
            .bind(auth::hash_token(&token))
            .bind(&app.bot_user_id)
            .bind(chat::now_iso())
            .execute(&mut *tx)
            .await
            .map_err(chat::db)?;
            tx.commit().await.map_err(chat::db)?;
            Ok(Json(json!({ "token": token })).into_response())
        }
        .await,
    )
}

/// `PUT /applications/{app}/guilds/{guild}`: add the bot to a guild. Only the guild owner can.
async fn add_bot_to_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id)): Path<(String, String)>,
) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let app = app_row(&state, &app_id).await?;
        let guild = chat::guild_row(&state, &guild_id).await?;
        if guild.owner_id != user.id {
            return Err(chat::no_access());
        }
        sqlx::query(&crate::db::sql("INSERT OR IGNORE INTO guild_members (guild_id, user_id, joined_at) VALUES (?, ?, ?)"))
            .bind(&guild_id)
            .bind(&app.bot_user_id)
            .bind(chat::now_iso())
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        let guild_payload = chat::guild_json(&state, guild, true).await?;
        let bot = bot_user(&state, &app.bot_user_id).await?;
        state.publish(
            vec![app.bot_user_id.clone()],
            "GUILD_CREATE",
            serde_json::to_value(&guild_payload).unwrap_or_default(),
        );
        let others: Vec<String> = chat::member_ids(&state, &guild_id)
            .await
            .map_err(chat::db)?
            .into_iter()
            .filter(|m| *m != app.bot_user_id)
            .collect();
        state.publish(others, "GUILD_MEMBER_ADD", json!({ "guild_id": guild_id, "user": bot }));
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await)
}

async fn remove_bot_from_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id)): Path<(String, String)>,
) -> Response {
    done(
        async {
            let user = chat::authed(&state, &headers).await?;
            let app = app_row(&state, &app_id).await?;
            let guild = chat::guild_row(&state, &guild_id).await?;
            if guild.owner_id != user.id {
                return Err(chat::no_access());
            }
            sqlx::query(&crate::db::sql(
                "DELETE FROM guild_members WHERE guild_id = ? AND user_id = ?",
            ))
            .bind(&guild_id)
            .bind(&app.bot_user_id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
            state.publish(
                vec![app.bot_user_id.clone()],
                "GUILD_DELETE",
                json!({ "id": guild_id }),
            );
            let remaining = chat::member_ids(&state, &guild_id)
                .await
                .map_err(chat::db)?;
            state.publish(
                remaining,
                "GUILD_MEMBER_REMOVE",
                json!({ "guild_id": guild_id, "user": { "id": app.bot_user_id } }),
            );
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        .await,
    )
}

// ---------------------------------------------------------------------------
// Application commands
// ---------------------------------------------------------------------------

/// A command as the bot sends it. Only chat-input commands (type 1) are supported.
#[derive(Deserialize)]
struct CommandBody {
    #[serde(rename = "type", default = "chat_input")]
    kind: u8,
    name: String,
    description: String,
    #[serde(default)]
    options: Vec<Value>,
}

fn chat_input() -> u8 {
    1
}

#[derive(sqlx::FromRow)]
struct CommandRow {
    id: String,
    version: String,
    guild_id: Option<String>,
    name: String,
    description: String,
    options: String,
}

/// Check a command and return its options as stored JSON text.
fn validate_command(body: &CommandBody) -> ApiResult<String> {
    if body.kind != 1 {
        return Err(bad(
            "only CHAT_INPUT (type 1) application commands are supported",
        ));
    }
    if body.name.is_empty() || body.name.chars().count() > 32 || !valid_name_chars(&body.name) {
        return Err(bad(
            "command name must be 1-32 lowercase letters, digits, - or _",
        ));
    }
    let description = body.description.chars().count();
    if !(1..=100).contains(&description) {
        return Err(bad("command description must be 1-100 characters"));
    }
    validate_options(&body.options, 0)?;
    serde_json::to_string(&body.options).map_err(|_| bad("invalid command options"))
}

fn validate_options(options: &[Value], depth: usize) -> ApiResult<()> {
    if options.len() > MAX_OPTIONS || depth > 3 {
        return Err(bad(format!(
            "a command takes at most {MAX_OPTIONS} options"
        )));
    }
    for option in options {
        let name = option
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let description = option
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = option.get("type").and_then(Value::as_u64).unwrap_or(0);
        if name.is_empty() || name.chars().count() > 32 || !valid_name_chars(name) {
            return Err(bad(
                "option names must be 1-32 lowercase letters, digits, - or _",
            ));
        }
        if !(1..=100).contains(&description.chars().count()) {
            return Err(bad("option descriptions must be 1-100 characters"));
        }
        if !(1..=11).contains(&kind) {
            return Err(bad("option type must be between 1 and 11"));
        }
        if let Some(children) = option.get("options").and_then(Value::as_array) {
            validate_options(children, depth + 1)?;
        }
    }
    Ok(())
}

fn command_json(row: &CommandRow, app_id: &str) -> Value {
    let mut out = json!({
        "id": row.id,
        "version": row.version,
        "application_id": app_id,
        "type": 1,
        "name": row.name,
        "description": row.description,
        "options": serde_json::from_str::<Value>(&row.options).unwrap_or(json!([])),
    });
    if let Some(guild) = &row.guild_id {
        out["guild_id"] = json!(guild);
    }
    out
}

async fn list_commands(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
) -> ApiResult<Vec<CommandRow>> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, version, guild_id, name, description, options FROM application_commands \
         WHERE application_id = ? AND guild_id IS ? ORDER BY CAST(id AS BIGINT)",
    ))
    .bind(&app.id)
    .bind(guild)
    .fetch_all(&state.db)
    .await
    .map_err(chat::db)
}

async fn guild_exists(state: &AppState, guild_id: &str) -> ApiResult<()> {
    chat::guild_row(state, guild_id).await.map(|_| ())
}

async fn insert_command(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
    body: &CommandBody,
) -> ApiResult<Value> {
    let options = validate_command(body)?;
    let count: (i64,) = sqlx::query_as(&crate::db::sql(
        "SELECT COUNT(*) FROM application_commands WHERE application_id = ? AND guild_id IS ?",
    ))
    .bind(&app.id)
    .bind(guild)
    .fetch_one(&state.db)
    .await
    .map_err(chat::db)?;
    if count.0 >= MAX_COMMANDS_PER_SCOPE {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            30032,
            "maximum number of application commands reached",
        ));
    }
    let id = auth::new_id();
    let version = auth::new_id();
    let inserted = sqlx::query(
        &crate::db::sql("INSERT INTO application_commands (id, application_id, guild_id, name, description, options, version, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
    )
    .bind(&id)
    .bind(&app.id)
    .bind(guild)
    .bind(&body.name)
    .bind(&body.description)
    .bind(&options)
    .bind(&version)
    .bind(chat::now_iso())
    .execute(&state.db)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(bad("a command with this name already exists"));
        }
        Err(e) => return Err(chat::db(e)),
    }
    Ok(command_json(
        &CommandRow {
            id,
            version,
            guild_id: guild.map(str::to_string),
            name: body.name.clone(),
            description: body.description.clone(),
            options,
        },
        &app.id,
    ))
}

async fn command_list_response(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
) -> ApiResult<Response> {
    let rows = list_commands(state, app, guild).await?;
    Ok(Json(
        rows.iter()
            .map(|r| command_json(r, &app.id))
            .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn overwrite_commands(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
    bodies: &[CommandBody],
) -> ApiResult<Response> {
    let mut seen = HashSet::new();
    let mut options = Vec::with_capacity(bodies.len());
    for body in bodies {
        options.push(validate_command(body)?);
        if !seen.insert(body.name.clone()) {
            return Err(bad("command names must be unique"));
        }
    }
    if bodies.len() as i64 > MAX_COMMANDS_PER_SCOPE {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            30032,
            "maximum number of application commands reached",
        ));
    }
    let mut tx = state.db.begin().await.map_err(chat::db)?;
    sqlx::query(&crate::db::sql(
        "DELETE FROM application_commands WHERE application_id = ? AND guild_id IS ?",
    ))
    .bind(&app.id)
    .bind(guild)
    .execute(&mut *tx)
    .await
    .map_err(chat::db)?;
    let mut out = Vec::with_capacity(bodies.len());
    let now = chat::now_iso();
    for (body, options) in bodies.iter().zip(options) {
        let id = auth::new_id();
        let version = auth::new_id();
        sqlx::query(
            &crate::db::sql("INSERT INTO application_commands (id, application_id, guild_id, name, description, options, version, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
        )
        .bind(&id).bind(&app.id).bind(guild).bind(&body.name).bind(&body.description)
        .bind(&options).bind(&version).bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(chat::db)?;
        out.push(command_json(
            &CommandRow {
                id,
                version,
                guild_id: guild.map(str::to_string),
                name: body.name.clone(),
                description: body.description.clone(),
                options,
            },
            &app.id,
        ));
    }
    tx.commit().await.map_err(chat::db)?;
    Ok(Json(out).into_response())
}

async fn edit_command_row(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
    command_id: &str,
    body: &CommandBody,
) -> ApiResult<Response> {
    let options = validate_command(body)?;
    let version = auth::new_id();
    let updated = sqlx::query(&crate::db::sql(
        "UPDATE application_commands SET name = ?, description = ?, options = ?, version = ? \
         WHERE id = ? AND application_id = ? AND guild_id IS ?",
    ))
    .bind(&body.name)
    .bind(&body.description)
    .bind(&options)
    .bind(&version)
    .bind(command_id)
    .bind(&app.id)
    .bind(guild)
    .execute(&state.db)
    .await;
    let updated = match updated {
        Ok(r) => r.rows_affected(),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(bad("a command with this name already exists"));
        }
        Err(e) => return Err(chat::db(e)),
    };
    if updated == 0 {
        return Err(unknown("application command", 10065));
    }
    Ok(Json(command_json(
        &CommandRow {
            id: command_id.to_string(),
            version,
            guild_id: guild.map(str::to_string),
            name: body.name.clone(),
            description: body.description.clone(),
            options,
        },
        &app.id,
    ))
    .into_response())
}

async fn delete_command_row(
    state: &AppState,
    app: &AppRow,
    guild: Option<&str>,
    command_id: &str,
) -> ApiResult<Response> {
    let deleted = sqlx::query(&crate::db::sql(
        "DELETE FROM application_commands WHERE id = ? AND application_id = ? AND guild_id IS ?",
    ))
    .bind(command_id)
    .bind(&app.id)
    .bind(guild)
    .execute(&state.db)
    .await
    .map_err(chat::db)?
    .rows_affected();
    if deleted == 0 {
        return Err(unknown("application command", 10065));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// Global commands (every guild the bot is in).

async fn list_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            command_list_response(&state, &app, None).await
        }
        .await,
    )
}

async fn create_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Json(body): Json<CommandBody>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            let created = insert_command(&state, &app, None, &body).await?;
            Ok((StatusCode::CREATED, Json(created)).into_response())
        }
        .await,
    )
}

async fn overwrite_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
    Json(bodies): Json<Vec<CommandBody>>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            overwrite_commands(&state, &app, None, &bodies).await
        }
        .await,
    )
}

async fn edit_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, command_id)): Path<(String, String)>,
    Json(body): Json<CommandBody>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            edit_command_row(&state, &app, None, &command_id, &body).await
        }
        .await,
    )
}

async fn delete_global(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, command_id)): Path<(String, String)>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            delete_command_row(&state, &app, None, &command_id).await
        }
        .await,
    )
}

// Guild commands (one guild only, visible only there).

async fn list_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id)): Path<(String, String)>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            guild_exists(&state, &guild_id).await?;
            command_list_response(&state, &app, Some(&guild_id)).await
        }
        .await,
    )
}

async fn create_guild_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id)): Path<(String, String)>,
    Json(body): Json<CommandBody>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            guild_exists(&state, &guild_id).await?;
            let created = insert_command(&state, &app, Some(&guild_id), &body).await?;
            Ok((StatusCode::CREATED, Json(created)).into_response())
        }
        .await,
    )
}

async fn overwrite_guild(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id)): Path<(String, String)>,
    Json(bodies): Json<Vec<CommandBody>>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            guild_exists(&state, &guild_id).await?;
            overwrite_commands(&state, &app, Some(&guild_id), &bodies).await
        }
        .await,
    )
}

async fn edit_guild_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id, command_id)): Path<(String, String, String)>,
    Json(body): Json<CommandBody>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            edit_command_row(&state, &app, Some(&guild_id), &command_id, &body).await
        }
        .await,
    )
}

async fn delete_guild_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app_id, guild_id, command_id)): Path<(String, String, String)>,
) -> Response {
    done(
        async {
            let app = commandable_app(&state, &headers, &app_id).await?;
            delete_command_row(&state, &app, Some(&guild_id), &command_id).await
        }
        .await,
    )
}

/// Discovery index the client uses for the composer: the commands of bots present in a guild,
/// plus guild-scoped commands registered for that guild.
/// Discovery index for a context. `guild` is set for guild channels; DMs pass `None` and the
/// channel id. Guild commands need the bot in the guild. Global commands need the bot in the guild
/// or DM, or the viewer having installed the application.
async fn index_for(
    state: &AppState,
    guild: Option<&str>,
    dm_channel: &str,
    user_id: &str,
) -> ApiResult<Response> {
    let rows: Vec<IndexRow> = match guild {
        Some(guild) => sqlx::query_as(
            &crate::db::sql("SELECT c.id, c.version, c.application_id, c.guild_id, c.name, c.description, c.options, a.name AS app_name \
             FROM application_commands c JOIN applications a ON a.id = c.application_id \
             WHERE (c.guild_id = ? OR c.guild_id IS NULL) AND (\
               EXISTS (SELECT 1 FROM guild_members gm WHERE gm.guild_id = ? AND gm.user_id = a.bot_user_id) \
               OR (c.guild_id IS NULL AND EXISTS (SELECT 1 FROM user_installs ui WHERE ui.application_id = a.id AND ui.user_id = ?))) \
             ORDER BY c.name, CAST(c.id AS BIGINT)"),
        )
        .bind(guild)
        .bind(guild)
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?,
        None => sqlx::query_as(
            &crate::db::sql("SELECT c.id, c.version, c.application_id, c.guild_id, c.name, c.description, c.options, a.name AS app_name \
             FROM application_commands c JOIN applications a ON a.id = c.application_id \
             WHERE c.guild_id IS NULL AND (\
               EXISTS (SELECT 1 FROM channel_recipients cr WHERE cr.channel_id = ? AND cr.user_id = a.bot_user_id) \
               OR EXISTS (SELECT 1 FROM user_installs ui WHERE ui.application_id = a.id AND ui.user_id = ?)) \
             ORDER BY c.name, CAST(c.id AS BIGINT)"),
        )
        .bind(dm_channel)
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?,
    };

    let mut commands = Vec::with_capacity(rows.len());
    let mut applications: Vec<Value> = Vec::new();
    let mut seen_apps = HashSet::new();
    for row in rows {
        commands.push(command_json(
            &CommandRow {
                id: row.id,
                version: row.version,
                guild_id: row.guild_id,
                name: row.name,
                description: row.description,
                options: row.options,
            },
            &row.application_id,
        ));
        if seen_apps.insert(row.application_id.clone()) {
            applications
                .push(json!({ "id": row.application_id, "name": row.app_name, "icon": null }));
        }
    }
    Ok(Json(json!({
        "version": 1,
        "application_commands": commands,
        "applications": applications,
    }))
    .into_response())
}

async fn guild_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    done(
        async {
            let user = chat::authed(&state, &headers).await?;
            chat::require_member(&state, &guild_id, &user.id).await?;
            index_for(&state, Some(&guild_id), "", &user.id).await
        }
        .await,
    )
}

async fn channel_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Response {
    done(
        async {
            let user = chat::authed(&state, &headers).await?;
            let channel = chat::channel_row(&state, &channel_id).await?;
            chat::require_channel(&state, &channel, &user.id).await?;
            index_for(&state, channel.guild_id.as_deref(), &channel.id, &user.id).await
        }
        .await,
    )
}

// ---------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------

/// Body of `POST /interactions`, as the client sends it. Unknown fields are ignored.
#[derive(Deserialize)]
struct InteractionRequest {
    #[serde(rename = "type")]
    kind: u8,
    application_id: String,
    channel_id: String,
    #[serde(default)]
    nonce: Option<Value>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    data: Value,
}

fn str_field<'a>(data: &'a Value, key: &str) -> ApiResult<&'a str> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("interaction data is missing {key}")))
}

fn nonce_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!text.is_empty() && text.chars().count() <= 64 && !text.chars().any(char::is_control))
        .then_some(text)
}

/// Accept the request from a user and forward it to the bot. The bot answers with its token.
/// Accept an interaction from a user and forward it to the bot. The bot answers with the token.
/// The bot must be in the guild or DM, or the user must have installed the application.
async fn create_interaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<InteractionRequest>,
) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let app = app_row(&state, &req.application_id).await?;
        let channel = chat::channel_row(&state, &req.channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        let bot_in_context = match &channel.guild_id {
            Some(guild) => is_guild_member(&state, guild, &app.bot_user_id).await?,
            None => is_recipient(&state, &channel.id, &app.bot_user_id).await?,
        };
        if !bot_in_context && !is_installed(&state, &user.id, &app.id).await? {
            return Err(unknown("application", 10002));
        }
        let nonce = req.nonce.as_ref().and_then(nonce_text);

        let mut message_json = None;
        let (bot_data, message_id) = match req.kind {
            // APPLICATION_COMMAND and APPLICATION_COMMAND_AUTOCOMPLETE share the command lookup.
            2 | 4 => {
                let command_id = str_field(&req.data, "id")?;
                let (version, name, command_guild): (String, String, Option<String>) = sqlx::query_as(
                    &crate::db::sql("SELECT version, name, guild_id FROM application_commands \
                     WHERE id = ? AND application_id = ? AND (guild_id IS NULL OR guild_id = ?)"),
                )
                .bind(command_id)
                .bind(&app.id)
                .bind(channel.guild_id.as_deref().unwrap_or(""))
                .fetch_optional(&state.db)
                .await
                .map_err(chat::db)?
                .ok_or_else(|| unknown("application command", 10065))?;
                if command_guild.is_some() && !bot_in_context {
                    return Err(unknown("application command", 10065));
                }
                let options = req.data.get("options").cloned().unwrap_or(json!([]));
                if !options.is_array() {
                    return Err(bad("command options must be an array"));
                }
                let mut data = json!({
                    "id": command_id,
                    "name": name,
                    "type": 1,
                    "version": version,
                    "options": options,
                });
                if let Some(guild) = command_guild {
                    data["guild_id"] = json!(guild);
                }
                (data, None)
            }
            3 => {
                let message_id = req.message_id.clone().ok_or_else(|| bad("component interactions need message_id"))?;
                let message = chat::message_by_id(&state, &message_id).await?;
                if message.channel_id != channel.id || message.author_id != app.bot_user_id {
                    return Err(chat::missing_message());
                }
                message_json = Some(chat::message_json(message));
                let custom_id = str_field(&req.data, "custom_id")?;
                let data = json!({
                    "custom_id": custom_id,
                    "component_type": req.data.get("component_type").and_then(Value::as_u64).unwrap_or(2),
                    "values": req.data.get("values").cloned().unwrap_or(json!([])),
                });
                (data, Some(message_id))
            }
            5 => {
                let modal_id = str_field(&req.data, "id")?;
                let (custom_id, application_id, invoker, channel_id): (String, String, String, String) = sqlx::query_as(
                    &crate::db::sql("SELECT m.custom_id, i.application_id, i.user_id, i.channel_id FROM modals m \
                     JOIN interactions i ON i.id = m.interaction_id WHERE m.id = ?"),
                )
                .bind(modal_id)
                .fetch_optional(&state.db)
                .await
                .map_err(chat::db)?
                .ok_or_else(|| unknown("interaction", 10062))?;
                if application_id != app.id || invoker != user.id || channel_id != channel.id {
                    return Err(unknown("interaction", 10062));
                }
                sqlx::query(&crate::db::sql("DELETE FROM modals WHERE id = ?"))
                    .bind(modal_id)
                    .execute(&state.db)
                    .await
                    .map_err(chat::db)?;
                let components = req.data.get("components").cloned().unwrap_or(json!([]));
                (json!({ "custom_id": custom_id, "components": components }), None)
            }
            _ => return Err(bad("unsupported interaction type")),
        };

        let interaction_id = auth::new_id();
        let token = auth::new_token();
        sqlx::query(
            &crate::db::sql("INSERT INTO interactions (id, token, application_id, kind, channel_id, guild_id, user_id, message_id, nonce, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"),
        )
        .bind(&interaction_id)
        .bind(&token)
        .bind(&app.id)
        .bind(req.kind as i64)
        .bind(&channel.id)
        .bind(channel.guild_id.as_deref())
        .bind(&user.id)
        .bind(&message_id)
        .bind(&nonce)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(chat::db)?;

        // Guild interactions carry a member; DM and user-install interactions carry the user.
        let mut payload = json!({
            "id": interaction_id,
            "application_id": app.id,
            "type": req.kind,
            "token": token,
            "version": 1,
            "channel_id": channel.id,
            "data": bot_data,
        });
        match &channel.guild_id {
            Some(guild) => {
                payload["guild_id"] = json!(guild);
                payload["member"] = json!({ "user": user });
            }
            None => payload["user"] = json!(user),
        }
        if let Some(message) = message_json {
            payload["message"] = serde_json::to_value(message).unwrap_or_default();
        }
        if req.kind == 4 {
            // Autocomplete: the bot answers with choices, delivered back as INTERACTION_AUTOCOMPLETE_RESULT.
            payload["data"]["options"] = req.data.get("options").cloned().unwrap_or(json!([]));
            state.publish(vec![app.bot_user_id.clone()], "INTERACTION_CREATE", payload);
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        state.publish(vec![app.bot_user_id.clone()], "INTERACTION_CREATE", payload);
        if let Some(nonce) = nonce {
            state.publish(vec![user.id.clone()], "INTERACTION_SUCCESS", json!({ "nonce": nonce }));
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await)
}

async fn interaction_by_id(state: &AppState, interaction_id: &str) -> ApiResult<InteractionRow> {
    sqlx::query_as(
        &crate::db::sql("SELECT id, token, application_id, kind, channel_id, user_id, message_id, nonce, responded, response_message_id, created_at \
         FROM interactions WHERE id = ?"),
    )
    .bind(interaction_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .ok_or_else(|| unknown("interaction", 10062))
}

/// The interaction token is the credential. Tokens expire after 15 minutes.
fn check_token(row: &InteractionRow, token: &str) -> ApiResult<()> {
    let created = chrono::DateTime::parse_from_rfc3339(&row.created_at).map_err(internal)?;
    let age = chrono::Utc::now()
        .signed_duration_since(created)
        .num_seconds();
    if row.token != token || age > TOKEN_TTL_SECS {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            50027,
            "Invalid Webhook Token",
        ));
    }
    Ok(())
}

async fn mark_responded(
    state: &AppState,
    interaction_id: &str,
    response_message_id: Option<&str>,
) -> ApiResult<()> {
    sqlx::query(&crate::db::sql("UPDATE interactions SET responded = 1, response_message_id = COALESCE(?, response_message_id) WHERE id = ?"))
        .bind(response_message_id)
        .bind(interaction_id)
        .execute(&state.db)
        .await
        .map_err(chat::db)?;
    Ok(())
}

/// Components as sent by a bot: an array of at most five action rows.
fn parse_components(value: Option<&Value>) -> ApiResult<Vec<Value>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| bad("components must be an array"))?;
    if rows.len() > MAX_COMPONENT_ROWS || rows.iter().any(|r| !r.is_object()) {
        return Err(bad(format!(
            "components must be at most {MAX_COMPONENT_ROWS} action rows"
        )));
    }
    if serde_json::to_vec(rows).map_err(internal)?.len() > MAX_COMPONENT_BYTES {
        return Err(bad("components are too large"));
    }
    Ok(rows.clone())
}

fn parse_content(value: Option<&Value>) -> ApiResult<Option<String>> {
    check_content(value.and_then(Value::as_str))
}

fn check_content(text: Option<&str>) -> ApiResult<Option<String>> {
    match text {
        None => Ok(None),
        Some(text) if text.chars().count() > MAX_CONTENT => Err(bad(format!(
            "content must be at most {MAX_CONTENT} characters"
        ))),
        Some(text) => Ok(Some(text.to_string())),
    }
}

fn parse_flags(value: Option<&Value>) -> u32 {
    value.and_then(Value::as_u64).unwrap_or(0) as u32 & FLAG_EPHEMERAL
}

pub async fn store_extras(
    state: &AppState,
    message_id: &str,
    flags: u32,
    components: &[Value],
    invoker: Option<&str>,
) -> Result<(), sqlx::Error> {
    let components = serde_json::to_string(components).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        &crate::db::sql("INSERT INTO message_extras (message_id, flags, components, invoker_id) VALUES (?, ?, ?, ?) \
         ON CONFLICT(message_id) DO UPDATE SET flags = excluded.flags, components = excluded.components, invoker_id = excluded.invoker_id"),
    )
    .bind(message_id)
    .bind(flags as i64)
    .bind(components)
    .bind(invoker)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Who should receive events for a message: the channel, or only the invoker for ephemeral ones.
async fn message_audience(
    state: &AppState,
    channel: &ChannelRow,
    flags: u32,
    invoker: Option<&str>,
) -> ApiResult<Vec<String>> {
    if flags & FLAG_EPHEMERAL != 0 {
        return Ok(invoker.map(|id| vec![id.to_string()]).unwrap_or_default());
    }
    chat::channel_audience(state, channel)
        .await
        .map_err(chat::db)
}

/// Post a message as a bot and announce it.
async fn post_bot_message(
    state: &AppState,
    channel: &ChannelRow,
    bot_id: &str,
    content: &str,
    components: &[Value],
    flags: u32,
    invoker: Option<&str>,
) -> ApiResult<Message> {
    if content.is_empty() && components.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            50006,
            "Cannot send an empty message",
        ));
    }
    let message_id = auth::new_id();
    sqlx::query(&crate::db::sql("INSERT INTO messages (id, channel_id, author_id, content, created_at) VALUES (?, ?, ?, ?, ?)"))
        .bind(&message_id)
        .bind(&channel.id)
        .bind(bot_id)
        .bind(content)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(chat::db)?;
    store_extras(state, &message_id, flags, components, invoker)
        .await
        .map_err(chat::db)?;
    let message = chat::message_json(chat::message_by_id(state, &message_id).await?);
    let targets = message_audience(state, channel, flags, invoker).await?;
    state.publish(
        targets,
        "MESSAGE_CREATE",
        serde_json::to_value(&message).unwrap_or_default(),
    );
    Ok(message)
}

/// Edit a bot message's content and/or components and announce it.
async fn edit_bot_message(
    state: &AppState,
    message_id: &str,
    content: Option<&str>,
    components: Option<&[Value]>,
) -> ApiResult<Message> {
    if let Some(content) = content {
        sqlx::query(&crate::db::sql(
            "UPDATE messages SET content = ?, edited_at = ? WHERE id = ?",
        ))
        .bind(content)
        .bind(chat::now_iso())
        .bind(message_id)
        .execute(&state.db)
        .await
        .map_err(chat::db)?;
    }
    if let Some(components) = components {
        let components_text = serde_json::to_string(components).unwrap_or_else(|_| "[]".into());
        let updated = sqlx::query(&crate::db::sql(
            "UPDATE message_extras SET components = ? WHERE message_id = ?",
        ))
        .bind(&components_text)
        .bind(message_id)
        .execute(&state.db)
        .await
        .map_err(chat::db)?
        .rows_affected();
        if updated == 0 {
            store_extras(state, message_id, 0, components, None)
                .await
                .map_err(chat::db)?;
        }
    }
    let row = chat::message_by_id(state, message_id).await?;
    let channel = chat::channel_row(state, &row.channel_id).await?;
    let extras: (i64, Option<String>) = sqlx::query_as(&crate::db::sql(
        "SELECT flags, invoker_id FROM message_extras WHERE message_id = ?",
    ))
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .unwrap_or((0, None));
    let message = chat::message_json(row);
    let targets = message_audience(state, &channel, extras.0 as u32, extras.1.as_deref()).await?;
    state.publish(
        targets,
        "MESSAGE_UPDATE",
        serde_json::to_value(&message).unwrap_or_default(),
    );
    Ok(message)
}

/// `POST /interactions/{id}/{token}/callback`. The bot's first response to an interaction.
#[derive(Deserialize)]
struct Callback {
    #[serde(rename = "type")]
    kind: u8,
    #[serde(default)]
    data: Option<Value>,
}

async fn callback(
    State(state): State<AppState>,
    Path((interaction_id, token)): Path<(String, String)>,
    Json(body): Json<Callback>,
) -> Response {
    done(async {
        let row = interaction_by_id(&state, &interaction_id).await?;
        check_token(&row, &token)?;
        if row.responded != 0 {
            return Err(api_error(StatusCode::BAD_REQUEST, 40060, "Interaction has already been acknowledged"));
        }
        // An autocomplete request is answered only with choices (type 8), and only once.
        if (row.kind == 4) != (body.kind == 8) {
            return Err(bad("autocomplete requests are answered with type 8, and only they are"));
        }
        let data = body.data.unwrap_or(Value::Null);
        let channel = chat::channel_row(&state, &row.channel_id).await?;
        let flags = parse_flags(data.get("flags"));
        match body.kind {
            // CHANNEL_MESSAGE_WITH_SOURCE
            4 => {
                let content = parse_content(data.get("content"))?.unwrap_or_default();
                let components = parse_components(data.get("components"))?;
                let invoker = (flags & FLAG_EPHEMERAL != 0).then_some(row.user_id.as_str());
                let message = post_bot_message(&state, &channel, &app_bot_id(&state, &row.application_id).await?, &content, &components, flags, invoker).await?;
                mark_responded(&state, &row.id, Some(&message.id)).await?;
            }
            // DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE and DEFERRED_UPDATE_MESSAGE: the bot follows up later.
            5 | 6 => mark_responded(&state, &row.id, None).await?,
            // UPDATE_MESSAGE: edit the message that carried the component.
            7 => {
                let message_id = row.message_id.clone().ok_or_else(|| bad("only component interactions can update a message"))?;
                let content = parse_content(data.get("content"))?;
                let components = match data.get("components") {
                    Some(value) => Some(parse_components(Some(value))?),
                    None => None,
                };
                edit_bot_message(&state, &message_id, content.as_deref(), components.as_deref()).await?;
                mark_responded(&state, &row.id, None).await?;
            }
            // MODAL: open a popup for the invoking user.
            9 => {
                let app = app_row(&state, &row.application_id).await?;
                let custom_id = str_field(&data, "custom_id")?.to_string();
                let title = str_field(&data, "title")?.to_string();
                if custom_id.is_empty() || custom_id.chars().count() > 100 || title.is_empty() || title.chars().count() > 45 {
                    return Err(bad("modal custom_id must be 1-100 characters and title 1-45"));
                }
                let components = data.get("components").cloned().unwrap_or(json!([]));
                if !components.is_array() {
                    return Err(bad("modal components must be an array"));
                }
                let modal_id = auth::new_id();
                sqlx::query(&crate::db::sql("INSERT INTO modals (id, interaction_id, custom_id, title, components, created_at) VALUES (?, ?, ?, ?, ?, ?)"))
                    .bind(&modal_id)
                    .bind(&row.id)
                    .bind(&custom_id)
                    .bind(&title)
                    .bind(components.to_string())
                    .bind(chat::now_iso())
                    .execute(&state.db)
                    .await
                    .map_err(chat::db)?;
                mark_responded(&state, &row.id, None).await?;
                state.publish(
                    vec![row.user_id.clone()],
                    "INTERACTION_MODAL_CREATE",
                    json!({
                        "id": modal_id,
                        "application": { "id": app.id },
                        "custom_id": custom_id,
                        "title": title,
                        "components": components,
                        "nonce": row.nonce,
                    }),
                );
            }
            // APPLICATION_COMMAND_AUTOCOMPLETE_RESULT: choices for the invoker's focused option.
            8 => {
                let choices = data.get("choices").cloned().unwrap_or(json!([]));
                let choices = choices
                    .as_array()
                    .filter(|list| list.len() <= 25)
                    .ok_or_else(|| bad("autocomplete choices must be an array of at most 25"))?;
                for choice in choices {
                    let name = choice.get("name").and_then(Value::as_str).unwrap_or_default();
                    let valid_name = !name.is_empty() && name.chars().count() <= 100;
                    let valid_value = matches!(choice.get("value"), Some(Value::String(v)) if v.chars().count() <= 100)
                        || matches!(choice.get("value"), Some(Value::Number(_)));
                    if !valid_name || !valid_value {
                        return Err(bad("each choice needs a name of 1-100 characters and a string or number value"));
                    }
                }
                mark_responded(&state, &row.id, None).await?;
                state.publish(
                    vec![row.user_id.clone()],
                    "INTERACTION_AUTOCOMPLETE_RESULT",
                    json!({ "nonce": row.nonce, "choices": choices }),
                );
            }
            _ => return Err(bad("unsupported callback type")),
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await)
}

async fn is_guild_member(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<bool> {
    let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
        "SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?",
    ))
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?;
    Ok(found.is_some())
}

async fn is_recipient(state: &AppState, channel_id: &str, user_id: &str) -> ApiResult<bool> {
    let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
        "SELECT 1 FROM channel_recipients WHERE channel_id = ? AND user_id = ?",
    ))
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?;
    Ok(found.is_some())
}

async fn is_installed(state: &AppState, user_id: &str, application_id: &str) -> ApiResult<bool> {
    let found: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
        "SELECT 1 FROM user_installs WHERE user_id = ? AND application_id = ?",
    ))
    .bind(user_id)
    .bind(application_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?;
    Ok(found.is_some())
}

/// The application named by its id, or by its bot user's id, so a client can install from a bot's profile.
async fn install_target(state: &AppState, id: &str) -> ApiResult<AppRow> {
    sqlx::query_as(&crate::db::sql(
        "SELECT id, owner_id, bot_user_id, name, description FROM applications WHERE id = ? OR bot_user_id = ?",
    ))
    .bind(id)
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .ok_or_else(|| unknown("Application", 10002))
}

/// `GET /users/@me/applications`: the applications installed for you, with their bot user ids.
async fn list_installed(State(state): State<AppState>, headers: HeaderMap) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(&crate::db::sql(
            "SELECT a.id, a.bot_user_id FROM user_installs i JOIN applications a ON a.id = i.application_id WHERE i.user_id = ?",
        ))
        .bind(&user.id)
        .fetch_all(&state.db)
        .await
        .map_err(chat::db)?;
        let out: Vec<Value> = rows
            .into_iter()
            .map(|(id, bot_user_id)| json!({"id": id, "bot_user_id": bot_user_id}))
            .collect();
        Ok(Json(out).into_response())
    }
    .await)
}

/// `PUT /users/@me/applications/{app}`: install an application for yourself.
async fn install_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(async {
        let user = chat::authed(&state, &headers).await?;
        let app = install_target(&state, &app_id).await?;
        sqlx::query(&crate::db::sql("INSERT OR IGNORE INTO user_installs (user_id, application_id, created_at) VALUES (?, ?, ?)"))
            .bind(&user.id)
            .bind(&app.id)
            .bind(chat::now_iso())
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await)
}

async fn uninstall_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    done(
        async {
            let user = chat::authed(&state, &headers).await?;
            let app = install_target(&state, &app_id).await?;
            sqlx::query(&crate::db::sql(
                "DELETE FROM user_installs WHERE user_id = ? AND application_id = ?",
            ))
            .bind(&user.id)
            .bind(&app.id)
            .execute(&state.db)
            .await
            .map_err(chat::db)?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        .await,
    )
}

async fn app_bot_id(state: &AppState, application_id: &str) -> ApiResult<String> {
    Ok(app_row(state, application_id).await?.bot_user_id)
}

/// Shared check for followups and `@original`: the token must belong to this application.
async fn interaction_for_webhook(
    state: &AppState,
    app_id: &str,
    token: &str,
) -> ApiResult<InteractionRow> {
    let row: InteractionRow = sqlx::query_as(
        &crate::db::sql("SELECT id, token, application_id, kind, channel_id, user_id, message_id, nonce, responded, response_message_id, created_at \
         FROM interactions WHERE token = ? AND application_id = ?"),
    )
    .bind(token)
    .bind(app_id)
    .fetch_optional(&state.db)
    .await
    .map_err(chat::db)?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10015, "Unknown Webhook"))?;
    check_token(&row, token)?;
    Ok(row)
}

#[derive(Deserialize)]
struct FollowupBody {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    flags: Option<u32>,
    #[serde(default)]
    components: Option<Value>,
}

/// `POST /webhooks/{app}/{token}`: a followup message after the first response.
async fn followup(
    State(state): State<AppState>,
    Path((app_id, token)): Path<(String, String)>,
    Json(body): Json<FollowupBody>,
) -> Response {
    done(
        async {
            let row = interaction_for_webhook(&state, &app_id, &token).await?;
            let channel = chat::channel_row(&state, &row.channel_id).await?;
            let content = check_content(body.content.as_deref())?.unwrap_or_default();
            let components = parse_components(body.components.as_ref())?;
            let flags = body.flags.unwrap_or(0) & FLAG_EPHEMERAL;
            let invoker = (flags != 0).then_some(row.user_id.as_str());
            let bot_id = app_bot_id(&state, &app_id).await?;
            let message = post_bot_message(
                &state,
                &channel,
                &bot_id,
                &content,
                &components,
                flags,
                invoker,
            )
            .await?;
            Ok(Json(message).into_response())
        }
        .await,
    )
}

#[derive(Deserialize)]
struct EditOriginalBody {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    components: Option<Value>,
}

/// `PATCH /webhooks/{app}/{token}/messages/@original`: edit the response, creating it if deferred.
async fn edit_original(
    State(state): State<AppState>,
    Path((app_id, token, message_id)): Path<(String, String, String)>,
    Json(body): Json<EditOriginalBody>,
) -> Response {
    done(
        async {
            if message_id != "@original" {
                return Err(chat::missing_message());
            }
            let row = interaction_for_webhook(&state, &app_id, &token).await?;
            let content = check_content(body.content.as_deref())?;
            let components = match body.components.as_ref() {
                Some(value) => Some(parse_components(Some(value))?),
                None => None,
            };
            match row.response_message_id.clone() {
                Some(existing) => {
                    let message = edit_bot_message(
                        &state,
                        &existing,
                        content.as_deref(),
                        components.as_deref(),
                    )
                    .await?;
                    Ok(Json(message).into_response())
                }
                None => {
                    // A deferred response with no message yet: this PATCH creates it.
                    let channel = chat::channel_row(&state, &row.channel_id).await?;
                    let bot_id = app_bot_id(&state, &app_id).await?;
                    let flags = 0;
                    let message = post_bot_message(
                        &state,
                        &channel,
                        &bot_id,
                        content.as_deref().unwrap_or_default(),
                        components.as_deref().unwrap_or_default(),
                        flags,
                        None,
                    )
                    .await?;
                    mark_responded(&state, &row.id, Some(&message.id)).await?;
                    Ok(Json(message).into_response())
                }
            }
        }
        .await,
    )
}

/// Visible-to-viewer filter for message history: drops ephemeral messages another user ran.
pub async fn hide_ephemeral(
    state: &AppState,
    viewer: &str,
    messages: &mut Vec<Message>,
) -> Result<(), sqlx::Error> {
    if messages.is_empty() {
        return Ok(());
    }
    let mut query: QueryBuilder<Any> = QueryBuilder::new(
        "SELECT message_id FROM message_extras WHERE (flags & 64) != 0 AND invoker_id IS NOT NULL AND invoker_id != ",
    );
    query.push_bind(viewer);
    query.push(" AND message_id IN (");
    let mut separated = query.separated(", ");
    for message in messages.iter() {
        separated.push_bind(message.id.clone());
    }
    separated.push_unseparated(")");
    let hidden: Vec<(String,)> = query.build_query_as().fetch_all(&state.db).await?;
    let hidden: HashSet<String> = hidden.into_iter().map(|(id,)| id).collect();
    messages.retain(|m| !hidden.contains(&m.id));
    Ok(())
}

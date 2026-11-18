//! Server onboarding: the questions a new member answers when joining. Managers set the prompts
//! with `PUT /guilds/{id}/onboarding` (`MANAGE_GUILD`). Members read them with `GET` and submit
//! their choices to `POST /guilds/{id}/onboarding-responses`, which Flint sends in the shape it
//! decodes. Prompts and options are stored as one JSON document per guild.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    api::{api_error, API_PREFIX},
    auth, chat,
    chat::ApiResult,
    perms,
    state::AppState,
};

pub const MAX_PROMPTS: usize = 50;
pub const MAX_OPTIONS: usize = 50;
const PROMPT_TITLE_MAX: usize = 100;
const OPTION_TITLE_MAX: usize = 50;
const OPTION_DESCRIPTION_MAX: usize = 100;
const EMOJI_NAME_MAX: usize = 32;
const MAX_SEEN_ENTRIES: usize = 500;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/onboarding"),
            get(read).put(write),
        )
        .route(
            &format!("{API_PREFIX}/guilds/{{guild_id}}/onboarding-responses"),
            post(respond),
        )
}

fn yes() -> bool {
    true
}

/// Emoji as Discord sends it. Only unicode emoji names are kept; custom emoji ids are ignored.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EmojiRef {
    #[serde(default)]
    pub name: Option<String>,
}

/// A prompt or option in the form Steel stores and returns.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Stored {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub emoji: Option<EmojiRef>,
    #[serde(default)]
    pub single_select: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "yes")]
    pub in_onboarding: bool,
    #[serde(default)]
    pub options: Vec<Stored>,
}

/// Body of `PUT /guilds/{id}/onboarding`. Ids are optional; new prompts and options get one.
#[derive(Deserialize)]
pub struct WritePrompt {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub single_select: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "yes")]
    pub in_onboarding: bool,
    #[serde(default)]
    pub options: Vec<WriteOption>,
}

#[derive(Deserialize)]
pub struct WriteOption {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub emoji: Option<EmojiRef>,
}

#[derive(Deserialize)]
pub struct WriteBody {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub prompts: Vec<WritePrompt>,
}

/// Body of `POST /guilds/{id}/onboarding-responses`.
#[derive(Deserialize, Default)]
pub struct RespondBody {
    #[serde(default)]
    pub onboarding_responses: Vec<String>,
    #[serde(default)]
    pub onboarding_prompts_seen: BTreeMap<String, u64>,
    #[serde(default)]
    pub onboarding_responses_seen: BTreeMap<String, u64>,
}

fn db(e: sqlx::Error) -> Response {
    chat::db(e)
}

fn invalid(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

fn check_text(value: &str, max: usize, what: &str) -> ApiResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(invalid(&format!("{what} must be 1-{max} characters")));
    }
    Ok(value.to_string())
}

fn check_description(value: Option<String>) -> ApiResult<Option<String>> {
    match value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        Some(v) if v.chars().count() > OPTION_DESCRIPTION_MAX => {
            Err(invalid("option description must be at most 100 characters"))
        }
        other => Ok(other),
    }
}

fn check_emoji(emoji: Option<EmojiRef>) -> ApiResult<Option<EmojiRef>> {
    let Some(name) = emoji.and_then(|e| e.name).map(|n| n.trim().to_string()) else {
        return Ok(None);
    };
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > EMOJI_NAME_MAX {
        return Err(invalid("emoji name must be at most 32 characters"));
    }
    Ok(Some(EmojiRef { name: Some(name) }))
}

/// Checks the whole document and fills in missing ids. Ids must be unique across prompts and options.
fn normalize(body: WriteBody) -> ApiResult<Vec<Stored>> {
    if body.prompts.len() > MAX_PROMPTS {
        return Err(invalid("a server can have at most 50 onboarding prompts"));
    }
    let mut seen = BTreeSet::new();
    let mut claim = |id: Option<String>| -> ApiResult<String> {
        let id = match id.map(|i| i.trim().to_string()).filter(|i| !i.is_empty()) {
            Some(id) => {
                if !id.chars().all(|c| c.is_ascii_digit()) || id.len() > 20 || id == "0" {
                    return Err(invalid("onboarding ids must be numeric strings"));
                }
                id
            }
            None => auth::new_id(),
        };
        if !seen.insert(id.clone()) {
            return Err(invalid("onboarding ids must be unique"));
        }
        Ok(id)
    };
    let mut prompts = Vec::with_capacity(body.prompts.len());
    for prompt in body.prompts {
        let id = claim(prompt.id)?;
        let title = check_text(&prompt.title, PROMPT_TITLE_MAX, "prompt title")?;
        if prompt.options.is_empty() || prompt.options.len() > MAX_OPTIONS {
            return Err(invalid("each prompt needs 1-50 options"));
        }
        let mut options = Vec::with_capacity(prompt.options.len());
        for option in prompt.options {
            let oid = claim(option.id)?;
            options.push(Stored {
                id: oid,
                title: check_text(&option.title, OPTION_TITLE_MAX, "option title")?,
                description: check_description(option.description)?,
                emoji: check_emoji(option.emoji)?,
                single_select: false,
                required: false,
                in_onboarding: true,
                options: Vec::new(),
            });
        }
        prompts.push(Stored {
            id,
            title,
            description: None,
            emoji: None,
            single_select: prompt.single_select,
            required: prompt.required,
            in_onboarding: prompt.in_onboarding,
            options,
        });
    }
    Ok(prompts)
}

/// Stored config for a guild: whether onboarding is on, and its prompts.
async fn config(state: &AppState, guild_id: &str) -> ApiResult<(bool, Vec<Stored>)> {
    let row: Option<(i64, String)> = sqlx::query_as(&crate::db::sql(
        "SELECT enabled, prompts FROM guild_onboarding WHERE guild_id = ?",
    ))
    .bind(guild_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?;
    Ok(match row {
        Some((enabled, prompts)) => (
            enabled != 0,
            serde_json::from_str(&prompts).unwrap_or_default(),
        ),
        None => (false, Vec::new()),
    })
}

/// Option ids the member chose that still exist in the guild's prompts.
async fn my_responses(
    state: &AppState,
    guild_id: &str,
    user_id: &str,
    prompts: &[Stored],
) -> ApiResult<Vec<String>> {
    let row: Option<(String,)> = sqlx::query_as(&crate::db::sql(
        "SELECT responses FROM user_onboarding WHERE user_id = ? AND guild_id = ?",
    ))
    .bind(user_id)
    .bind(guild_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db)?;
    let stored: Vec<String> = row
        .map(|(r,)| serde_json::from_str(&r).unwrap_or_default())
        .unwrap_or_default();
    let known: BTreeSet<&str> = prompts
        .iter()
        .flat_map(|p| p.options.iter().map(|o| o.id.as_str()))
        .collect();
    Ok(stored
        .into_iter()
        .filter(|id| known.contains(id.as_str()))
        .collect())
}

fn document(guild_id: &str, enabled: bool, prompts: &[Stored], responses: &[String]) -> Value {
    let prompts: Vec<Value> = prompts
        .iter()
        .map(|p| {
            let options: Vec<Value> = p
                .options
                .iter()
                .map(|o| {
                    json!({
                        "id": o.id,
                        "title": o.title,
                        "description": o.description,
                        "emoji": o.emoji.as_ref().and_then(|e| e.name.clone()).map(|name| json!({"id": null, "name": name})),
                    })
                })
                .collect();
            json!({
                "id": p.id,
                "title": p.title,
                "single_select": p.single_select,
                "required": p.required,
                "in_onboarding": p.in_onboarding,
                "options": options,
            })
        })
        .collect();
    json!({
        "guild_id": guild_id,
        "enabled": enabled,
        "prompts": prompts,
        "default_channel_ids": [],
        "responses": responses,
    })
}

async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let (enabled, prompts) = config(&state, &guild_id).await?;
        let responses = my_responses(&state, &guild_id, &user.id, &prompts).await?;
        Ok(Json(document(&guild_id, enabled, &prompts, &responses)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn write(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<WriteBody>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        perms::require_guild(&state, &guild_id, &user.id, perms::MANAGE_GUILD).await?;
        let enabled = body.enabled;
        let prompts = normalize(body)?;
        let stored = serde_json::to_string(&prompts).map_err(crate::api::internal)?;
        sqlx::query(&crate::db::sql(
            "INSERT INTO guild_onboarding (guild_id, enabled, prompts, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (guild_id) DO UPDATE SET enabled = excluded.enabled, prompts = excluded.prompts, updated_at = excluded.updated_at",
        ))
        .bind(&guild_id)
        .bind(i64::from(enabled))
        .bind(&stored)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        let responses = my_responses(&state, &guild_id, &user.id, &prompts).await?;
        Ok(Json(document(&guild_id, enabled, &prompts, &responses)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Merges seen markers into the stored map. Values are epoch milliseconds; newer values win.
fn merge_seen(existing: &str, incoming: &BTreeMap<String, u64>) -> ApiResult<String> {
    let mut map: BTreeMap<String, u64> = serde_json::from_str(existing).unwrap_or_default();
    for (key, at) in incoming {
        map.insert(key.clone(), *at);
    }
    if map.len() > MAX_SEEN_ENTRIES {
        return Err(invalid("too many onboarding seen markers"));
    }
    serde_json::to_string(&map).map_err(crate::api::internal)
}

async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(guild_id): Path<String>,
    Json(body): Json<RespondBody>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        chat::require_member(&state, &guild_id, &user.id).await?;
        let (enabled, prompts) = config(&state, &guild_id).await?;
        if !enabled {
            return Err(invalid("onboarding is not enabled for this server"));
        }

        // Every chosen option must exist, and each prompt's answer must fit its rules.
        let chosen: BTreeSet<String> = body.onboarding_responses.iter().cloned().collect();
        for prompt in &prompts {
            let picked = prompt
                .options
                .iter()
                .filter(|o| chosen.contains(&o.id))
                .count();
            if prompt.single_select && picked > 1 {
                return Err(invalid("only one option can be chosen for this prompt"));
            }
            if prompt.in_onboarding && prompt.required && picked == 0 {
                return Err(invalid("a required onboarding prompt was not answered"));
            }
        }
        let known: BTreeSet<&str> = prompts
            .iter()
            .flat_map(|p| p.options.iter().map(|o| o.id.as_str()))
            .collect();
        if chosen.iter().any(|id| !known.contains(id.as_str())) {
            return Err(invalid("unknown onboarding option"));
        }

        let existing: Option<(String, String, String)> = sqlx::query_as(&crate::db::sql(
            "SELECT prompts_seen, options_seen, responses FROM user_onboarding WHERE user_id = ? AND guild_id = ?",
        ))
        .bind(&user.id)
        .bind(&guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let (prompts_seen, options_seen) = match &existing {
            Some((p, o, _)) => (p.clone(), o.clone()),
            None => ("{}".to_string(), "{}".to_string()),
        };
        let prompts_seen = merge_seen(&prompts_seen, &body.onboarding_prompts_seen)?;
        let options_seen = merge_seen(&options_seen, &body.onboarding_responses_seen)?;
        let responses = serde_json::to_string(&chosen.iter().collect::<Vec<_>>())
            .map_err(crate::api::internal)?;

        sqlx::query(&crate::db::sql(
            "INSERT INTO user_onboarding (user_id, guild_id, responses, prompts_seen, options_seen, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (user_id, guild_id) DO UPDATE SET responses = excluded.responses, prompts_seen = excluded.prompts_seen, \
             options_seen = excluded.options_seen, updated_at = excluded.updated_at",
        ))
        .bind(&user.id)
        .bind(&guild_id)
        .bind(&responses)
        .bind(&prompts_seen)
        .bind(&options_seen)
        .bind(chat::now_iso())
        .execute(&state.db)
        .await
        .map_err(db)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

use axum::{
    extract::{State, WebSocketUpgrade},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use flint_protocol::{ApiError, LoginRequest, RegisterRequest, TokenResponse, User};

use crate::{
    auth, chat, dm, gateway, members, pins, polls, presence, reactions, search, state::AppState,
    uploads,
};

pub const API_PREFIX: &str = "/api/v10";

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { "Steel. Strike your own spark." }))
        .route("/healthz", get(|| async { "ok" }))
        .route(&format!("{API_PREFIX}/gateway"), get(gateway_info))
        .route(&format!("{API_PREFIX}/auth/register"), post(register))
        .route(&format!("{API_PREFIX}/auth/login"), post(login))
        .route(&format!("{API_PREFIX}/users/@me"), get(me))
        .route("/gateway", get(gateway_socket))
        .merge(chat::routes())
        .merge(dm::routes())
        .merge(presence::routes())
        .merge(uploads::routes())
        .merge(members::routes())
        .merge(reactions::routes())
        .merge(pins::routes())
        .merge(polls::routes())
        .merge(search::routes())
        .merge(crate::profile::routes())
        .layer(axum::extract::DefaultBodyLimit::max(
            uploads::max_file_bytes() as usize * uploads::MAX_FILES + 1024 * 1024,
        ))
        .with_state(state)
}

pub fn api_error(status: StatusCode, code: u32, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            code,
            message: message.into(),
        }),
    )
        .into_response()
}

pub fn internal(err: impl std::fmt::Display) -> Response {
    tracing::error!("internal error: {err}");
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        0,
        "internal server error",
    )
}

fn valid_username(name: &str) -> bool {
    let len = name.chars().count();
    (2..=32).contains(&len)
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

async fn gateway_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "url": "/gateway" }))
}

async fn register(State(state): State<AppState>, Json(req): Json<RegisterRequest>) -> Response {
    if !valid_username(&req.username) {
        return api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "username must be 2-32 letters, digits, _ . or -",
        );
    }
    if req.password.chars().count() < 8 {
        return api_error(
            StatusCode::BAD_REQUEST,
            50035,
            "password must be at least 8 characters",
        );
    }
    let hash = match auth::hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => return internal(e),
    };
    let id = auth::new_id();
    let now = chrono::Utc::now().to_rfc3339();
    let inserted = sqlx::query(
        "INSERT INTO users (id, username, email, password_hash, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&req.username)
    .bind(req.email.as_deref().filter(|e| !e.is_empty()))
    .bind(&hash)
    .bind(&now)
    .execute(&state.db)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return api_error(
                StatusCode::BAD_REQUEST,
                50035,
                "that username or email is already taken",
            );
        }
        Err(e) => return internal(e),
    }
    match issue_token(&state, &id).await {
        Ok(token) => (
            StatusCode::CREATED,
            Json(TokenResponse {
                token,
                user: User {
                    id,
                    username: req.username,
                    global_name: None,
                    avatar: None,
                    bot: false,
                },
            }),
        )
            .into_response(),
        Err(e) => internal(e),
    }
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Response {
    let row: Option<(String, String, String)> = match sqlx::query_as(
        "SELECT id, username, password_hash FROM users WHERE username = ? OR email = ?",
    )
    .bind(&req.login)
    .bind(&req.login)
    .fetch_optional(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    // Same error for unknown user and wrong password, so logins can't be used to enumerate accounts.
    let Some((id, username, hash)) = row else {
        return api_error(StatusCode::UNAUTHORIZED, 0, "invalid login or password");
    };
    if !auth::verify_password(&req.password, &hash) {
        return api_error(StatusCode::UNAUTHORIZED, 0, "invalid login or password");
    }
    match issue_token(&state, &id).await {
        Ok(token) => Json(TokenResponse {
            token,
            user: User {
                id,
                username,
                global_name: None,
                avatar: None,
                bot: false,
            },
        })
        .into_response(),
        Err(e) => internal(e),
    }
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match user_from_headers(&state, &headers).await {
        Ok(Some(user)) => Json(user).into_response(),
        Ok(None) => api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"),
        Err(e) => internal(e),
    }
}

async fn gateway_socket(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| gateway::run(state, socket))
}

/// Resolve the bearer token in `Authorization` to a user, if valid.
pub async fn user_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<User>, sqlx::Error> {
    let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return Ok(None);
    };
    let token = value.strip_prefix("Bearer ").unwrap_or(value);
    user_from_token(state, token).await
}

pub async fn user_from_token(state: &AppState, token: &str) -> Result<Option<User>, sqlx::Error> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT u.id, u.username FROM tokens t JOIN users u ON u.id = t.user_id WHERE t.token_hash = ?",
    )
    .bind(auth::hash_token(token))
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(id, username)| User {
        id,
        username,
        global_name: None,
        avatar: None,
        bot: false,
    }))
}

async fn issue_token(state: &AppState, user_id: &str) -> Result<String, sqlx::Error> {
    let token = auth::new_token();
    sqlx::query("INSERT INTO tokens (token_hash, user_id, created_at) VALUES (?, ?, ?)")
        .bind(auth::hash_token(&token))
        .bind(user_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.db)
        .await?;
    Ok(token)
}

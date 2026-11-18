//! DMs, group DMs, and the relationship (friends and blocking) API.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use flint_protocol::{
    channel_type, relationship_type, Channel, CreateDm, Relationship, SendFriendRequest,
    UpdateRelationship, User,
};
use serde_json::json;

use crate::{
    api::{api_error, user_from_headers, API_PREFIX},
    auth, chat,
    state::AppState,
};

type ApiResult<T> = Result<T, Response>;

const MAX_GROUP_RECIPIENTS: usize = 9;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/users/@me/channels"),
            get(my_dms).post(create_dm),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/relationships"),
            get(my_relationships).post(send_friend_request),
        )
        .route(
            &format!("{API_PREFIX}/users/@me/relationships/{{user_id}}"),
            put(update_relationship).delete(remove_relationship),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

async fn authed(state: &AppState, headers: &HeaderMap) -> ApiResult<User> {
    user_from_headers(state, headers)
        .await
        .map_err(db)?
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, 0, "401: Unauthorized"))
}

/// DM channels the user is in, with recipients.
pub async fn private_channels_for_user(
    state: &AppState,
    user_id: &str,
) -> Result<Vec<Channel>, sqlx::Error> {
    let rows: Vec<(String, i64, Option<String>, Option<String>)> = sqlx::query_as(
        &crate::db::sql("SELECT c.id, c.kind, c.name, c.owner_id FROM channels c JOIN channel_recipients r ON r.channel_id = c.id \
         WHERE r.user_id = ? AND c.guild_id IS NULL ORDER BY CAST(c.id AS BIGINT)"),
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, kind, name, _owner) in rows {
        out.push(dm_channel(state, id, kind, name).await?);
    }
    Ok(out)
}

async fn dm_channel(
    state: &AppState,
    id: String,
    kind: i64,
    name: Option<String>,
) -> Result<Channel, sqlx::Error> {
    let users: Vec<(String, String)> = sqlx::query_as(
        &crate::db::sql("SELECT u.id, u.username FROM channel_recipients r JOIN users u ON u.id = r.user_id WHERE r.channel_id = ? ORDER BY CAST(u.id AS BIGINT)"),
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    Ok(Channel {
        id,
        kind: kind as u8,
        guild_id: None,
        name,
        position: 0,
        topic: None,
        parent_id: None,
        permission_overwrites: Vec::new(),
        recipients: users
            .into_iter()
            .map(|(id, username)| chat::plain_user(id, username))
            .collect(),
        thread_metadata: None,
        owner_id: None,
    })
}

pub async fn is_blocked_either_way(
    state: &AppState,
    a: &str,
    b: &str,
) -> Result<bool, sqlx::Error> {
    let found: Option<(i64,)> = sqlx::query_as(
        &crate::db::sql("SELECT 1 FROM relationships WHERE kind = ? AND ((user_id = ? AND target_id = ?) OR (user_id = ? AND target_id = ?))"),
    )
    .bind(relationship_type::BLOCKED as i64)
    .bind(a).bind(b).bind(b).bind(a)
    .fetch_optional(&state.db)
    .await?;
    Ok(found.is_some())
}

async fn my_dms(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        Ok(Json(
            private_channels_for_user(&state, &user.id)
                .await
                .map_err(db)?,
        )
        .into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Open a 1:1 DM (`recipient_id`) or create a group DM (`recipients`, at least two others).
async fn create_dm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateDm>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let mut others: Vec<String> = req.recipients.clone();
        if let Some(id) = req.recipient_id.clone() {
            others.push(id);
        }
        others.retain(|id| *id != user.id);
        others.sort();
        others.dedup();
        if others.is_empty() {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "a DM needs at least one other recipient"));
        }
        if others.len() > MAX_GROUP_RECIPIENTS {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "too many recipients"));
        }

        for other in &others {
            let exists: Option<(i64,)> = sqlx::query_as(&crate::db::sql("SELECT 1 FROM users WHERE id = ?"))
                .bind(other)
                .fetch_optional(&state.db)
                .await
                .map_err(db)?;
            if exists.is_none() {
                return Err(api_error(StatusCode::NOT_FOUND, 10013, "Unknown user"));
            }
            if is_blocked_either_way(&state, &user.id, other).await.map_err(db)? {
                return Err(api_error(StatusCode::FORBIDDEN, 50007, "Cannot send messages to this user"));
            }
        }

        let (kind, name) = if others.len() == 1 {
            (channel_type::DM, None)
        } else {
            (channel_type::GROUP_DM, req.name.clone().filter(|n| !n.trim().is_empty()))
        };

        // Reuse the existing 1:1 DM between the same two people.
        if kind == channel_type::DM {
            let existing: Option<(String,)> = sqlx::query_as(
                &crate::db::sql("SELECT c.id FROM channels c \
                 JOIN channel_recipients a ON a.channel_id = c.id AND a.user_id = ? \
                 JOIN channel_recipients b ON b.channel_id = c.id AND b.user_id = ? \
                 WHERE c.guild_id IS NULL AND c.kind = ?"),
            )
            .bind(&user.id).bind(&others[0]).bind(channel_type::DM as i64)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
            if let Some((id,)) = existing {
                let channel = dm_channel(&state, id, kind as i64, None).await.map_err(db)?;
                return Ok((StatusCode::OK, Json(channel)).into_response());
            }
        }

        let channel_id = auth::new_id();
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = state.db.begin().await.map_err(db)?;
        sqlx::query(&crate::db::sql("INSERT INTO channels (id, guild_id, kind, name, position, owner_id, created_at) VALUES (?, NULL, ?, ?, 0, ?, ?)"))
            .bind(&channel_id).bind(kind as i64).bind(&name).bind(&user.id).bind(&now)
            .execute(&mut *tx).await.map_err(db)?;
        for member in std::iter::once(&user.id).chain(others.iter()) {
            sqlx::query(&crate::db::sql("INSERT INTO channel_recipients (channel_id, user_id) VALUES (?, ?)"))
                .bind(&channel_id).bind(member)
                .execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;

        let channel = dm_channel(&state, channel_id, kind as i64, name).await.map_err(db)?;
        let targets: Vec<String> = std::iter::once(user.id.clone()).chain(others.clone()).collect();
        state.publish(targets, "CHANNEL_CREATE", serde_json::to_value(&channel).unwrap_or_default());
        Ok((StatusCode::OK, Json(channel)).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

// ---------------------------------------------------------------------------
// Relationships
// ---------------------------------------------------------------------------

async fn relationship_kind(
    state: &AppState,
    user: &str,
    target: &str,
) -> Result<Option<u8>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(&crate::db::sql(
        "SELECT kind FROM relationships WHERE user_id = ? AND target_id = ?",
    ))
    .bind(user)
    .bind(target)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(k,)| k as u8))
}

async fn set_pair(
    state: &AppState,
    user: &str,
    target: &str,
    mine: Option<u8>,
    theirs: Option<u8>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.db.begin().await?;
    for (from, to, kind) in [(user, target, mine), (target, user, theirs)] {
        match kind {
            Some(k) => {
                sqlx::query(&crate::db::sql("INSERT INTO relationships (user_id, target_id, kind, created_at) VALUES (?, ?, ?, ?) \
                             ON CONFLICT(user_id, target_id) DO UPDATE SET kind = excluded.kind"))
                    .bind(from).bind(to).bind(k as i64).bind(&now)
                    .execute(&mut *tx).await?;
            }
            None => {
                sqlx::query(&crate::db::sql(
                    "DELETE FROM relationships WHERE user_id = ? AND target_id = ?",
                ))
                .bind(from)
                .bind(to)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await
}

async fn relationship_view(
    state: &AppState,
    owner: &str,
    target: &str,
    kind: u8,
) -> ApiResult<Relationship> {
    let row: Option<(String,)> =
        sqlx::query_as(&crate::db::sql("SELECT username FROM users WHERE id = ?"))
            .bind(target)
            .fetch_optional(&state.db)
            .await
            .map_err(db)?;
    let (username,) = row.ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10013, "Unknown user"))?;
    let _ = owner;
    Ok(Relationship {
        id: target.to_string(),
        kind,
        user: chat::plain_user(target.to_string(), username),
    })
}

async fn my_relationships(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result: ApiResult<Response> = async {
        let user = authed(&state, &headers).await?;
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            &crate::db::sql("SELECT r.target_id, u.username, r.kind FROM relationships r JOIN users u ON u.id = r.target_id WHERE r.user_id = ?"),
        )
        .bind(&user.id)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        let out: Vec<Relationship> = rows
            .into_iter()
            .map(|(id, username, kind)| Relationship { id: id.clone(), kind: kind as u8, user: chat::plain_user(id, username) })
            .collect();
        Ok(Json(out).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

async fn publish_relationship(state: &AppState, owner: &str, rel: &Relationship) {
    state.publish(
        vec![owner.to_string()],
        "RELATIONSHIP_ADD",
        serde_json::to_value(rel).unwrap_or_default(),
    );
}

/// Send a friend request by username.
async fn send_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendFriendRequest>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = authed(&state, &headers).await?;
        let target: Option<(String, String)> = sqlx::query_as(&crate::db::sql(
            "SELECT id, username FROM users WHERE lower(username) = lower(?)",
        ))
        .bind(req.username.trim())
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let (target_id, target_name) =
            target.ok_or_else(|| api_error(StatusCode::NOT_FOUND, 10013, "Unknown user"))?;
        if target_id == me.id {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                80000,
                "You cannot friend yourself",
            ));
        }
        let mine = relationship_kind(&state, &me.id, &target_id)
            .await
            .map_err(db)?;
        let theirs = relationship_kind(&state, &target_id, &me.id)
            .await
            .map_err(db)?;
        if mine == Some(relationship_type::BLOCKED) || theirs == Some(relationship_type::BLOCKED) {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                80000,
                "Cannot send a friend request to this user",
            ));
        }
        // Already friends, or they already asked us: accept.
        if mine == Some(relationship_type::INCOMING_REQUEST)
            || theirs == Some(relationship_type::OUTGOING_REQUEST)
        {
            set_pair(
                &state,
                &me.id,
                &target_id,
                Some(relationship_type::FRIEND),
                Some(relationship_type::FRIEND),
            )
            .await
            .map_err(db)?;
        } else if mine.is_none() {
            set_pair(
                &state,
                &me.id,
                &target_id,
                Some(relationship_type::OUTGOING_REQUEST),
                Some(relationship_type::INCOMING_REQUEST),
            )
            .await
            .map_err(db)?;
        }
        let mine_now = relationship_kind(&state, &me.id, &target_id)
            .await
            .map_err(db)?
            .unwrap_or(0);
        let rel = Relationship {
            id: target_id.clone(),
            kind: mine_now,
            user: chat::plain_user(target_id.clone(), target_name),
        };
        publish_relationship(&state, &me.id, &rel).await;
        let theirs_now = relationship_kind(&state, &target_id, &me.id)
            .await
            .map_err(db)?
            .unwrap_or(0);
        let their_view = Relationship {
            id: me.id.clone(),
            kind: theirs_now,
            user: me.clone(),
        };
        publish_relationship(&state, &target_id, &their_view).await;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `PUT` with no body or `type: 1` accepts an incoming request; `type: 2` blocks the user.
async fn update_relationship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_id): Path<String>,
    body: Option<Json<UpdateRelationship>>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = authed(&state, &headers).await?;
        if target_id == me.id {
            return Err(api_error(StatusCode::BAD_REQUEST, 80000, "Invalid user"));
        }
        let requested = body.and_then(|Json(b)| b.kind);
        let theirs_before = relationship_kind(&state, &target_id, &me.id)
            .await
            .map_err(db)?;
        let mine_before = relationship_kind(&state, &me.id, &target_id)
            .await
            .map_err(db)?;

        let (mine, theirs) = match requested {
            Some(t) if t == relationship_type::BLOCKED => (Some(relationship_type::BLOCKED), None),
            _ => {
                if mine_before == Some(relationship_type::INCOMING_REQUEST) {
                    (
                        Some(relationship_type::FRIEND),
                        Some(relationship_type::FRIEND),
                    )
                } else {
                    return Err(api_error(
                        StatusCode::NOT_FOUND,
                        10010,
                        "No pending request from this user",
                    ));
                }
            }
        };
        set_pair(&state, &me.id, &target_id, mine, theirs)
            .await
            .map_err(db)?;
        let _ = theirs_before;
        publish_pair(&state, &me.id, &target_id).await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// `DELETE` removes a friend, cancels an outgoing request, declines an incoming one, or unblocks.
async fn remove_relationship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_id): Path<String>,
) -> Response {
    let result: ApiResult<Response> = async {
        let me = authed(&state, &headers).await?;
        let mine = relationship_kind(&state, &me.id, &target_id)
            .await
            .map_err(db)?;
        if mine.is_none() {
            return Err(api_error(
                StatusCode::NOT_FOUND,
                10010,
                "Unknown relationship",
            ));
        }
        set_pair(&state, &me.id, &target_id, None, None)
            .await
            .map_err(db)?;
        state.publish(
            vec![me.id.clone()],
            "RELATIONSHIP_REMOVE",
            json!({ "id": target_id, "type": mine }),
        );
        if mine == Some(relationship_type::FRIEND)
            || mine == Some(relationship_type::OUTGOING_REQUEST)
            || mine == Some(relationship_type::INCOMING_REQUEST)
        {
            state.publish(
                vec![target_id.clone()],
                "RELATIONSHIP_REMOVE",
                json!({ "id": me.id }),
            );
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// After a change between `me` and `target`, push each side's view of the pair.
async fn publish_pair(state: &AppState, me: &str, target: &str) -> Result<(), Response> {
    let my_kind = relationship_kind(state, me, target)
        .await
        .map_err(db)?
        .unwrap_or(0);
    let their_kind = relationship_kind(state, target, me)
        .await
        .map_err(db)?
        .unwrap_or(0);
    let my_view = relationship_view(state, me, target, my_kind).await?;
    let their_view = relationship_view(state, target, me, their_kind).await?;
    state.publish(
        vec![me.to_string()],
        "RELATIONSHIP_UPDATE",
        serde_json::to_value(&my_view).unwrap_or_default(),
    );
    state.publish(
        vec![target.to_string()],
        "RELATIONSHIP_UPDATE",
        serde_json::to_value(&their_view).unwrap_or_default(),
    );
    Ok(())
}

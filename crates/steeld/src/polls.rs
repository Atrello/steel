//! Polls. A poll rides on a message: the author creates it with `poll` on a message, anyone who can see
//! the channel votes with `PUT .../polls/{id}/answers/@me`, and the author ends it early with
//! `POST .../polls/{id}/expire`. Votes fire `MESSAGE_POLL_VOTE_ADD` and `MESSAGE_POLL_VOTE_REMOVE`.

use std::collections::{BTreeSet, HashSet};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{post, put},
    Json, Router,
};
use flint_protocol::{
    CreatePoll, Message, Poll, PollAnswer, PollCount, PollMedia, PollResults, PollText, PollVote,
};
use serde_json::json;

use crate::{
    api::{api_error, API_PREFIX},
    chat::{self, ApiResult},
    reactions,
    state::AppState,
    uploads,
};

pub const MAX_ANSWERS: usize = 10;
pub const MAX_QUESTION_CHARS: usize = 300;
pub const MAX_ANSWER_CHARS: usize = 55;
/// Hours, as the client sends them. Discord allows up to 768.
pub const MAX_DURATION_HOURS: u16 = 768;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/polls/{{message_id}}/answers/@me"),
            put(vote),
        )
        .route(
            &format!("{API_PREFIX}/channels/{{channel_id}}/polls/{{message_id}}/expire"),
            post(expire),
        )
}

fn db(e: sqlx::Error) -> Response {
    crate::api::internal(e)
}

fn invalid(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, 50035, message)
}

/// Checks a poll in a create-message request before anything is written.
pub fn validate(poll: &CreatePoll) -> ApiResult<()> {
    let question = poll.question.text.trim();
    if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
        return Err(invalid("poll question must be 1-300 characters"));
    }
    if !(2..=MAX_ANSWERS).contains(&poll.answers.len()) {
        return Err(invalid("a poll needs 2 to 10 answers"));
    }
    for answer in &poll.answers {
        let text = answer.poll_media.text.trim();
        if text.chars().count() > MAX_ANSWER_CHARS {
            return Err(invalid("poll answers must be at most 55 characters"));
        }
        if text.is_empty() && answer.poll_media.emoji.is_none() {
            return Err(invalid("a poll answer needs text or an emoji"));
        }
    }
    if !(1..=MAX_DURATION_HOURS).contains(&poll.duration) {
        return Err(invalid("poll duration must be 1 to 768 hours"));
    }
    if poll.layout_type != 1 {
        return Err(invalid("unsupported poll layout"));
    }
    Ok(())
}

/// Stores a validated poll for `message_id`. The poll expires `duration` hours after `now`.
pub async fn create(
    state: &AppState,
    message_id: &str,
    poll: &CreatePoll,
) -> Result<(), sqlx::Error> {
    let expires_at = (chrono::Utc::now() + chrono::Duration::hours(i64::from(poll.duration)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query(&crate::db::sql("INSERT INTO polls (message_id, question, expires_at, multiselect, finalized) VALUES (?, ?, ?, ?, 0)"))
        .bind(message_id)
        .bind(poll.question.text.trim())
        .bind(expires_at)
        .bind(i64::from(poll.allow_multiselect))
        .execute(&state.db)
        .await?;
    for (index, answer) in poll.answers.iter().enumerate() {
        let emoji = answer
            .poll_media
            .emoji
            .as_ref()
            .and_then(|e| match (&e.id, &e.name) {
                (Some(id), name) => Some(format!("{}:{id}", name.clone().unwrap_or_default())),
                (None, Some(name)) if !name.is_empty() => Some(name.clone()),
                _ => None,
            });
        sqlx::query(&crate::db::sql(
            "INSERT INTO poll_answers (message_id, answer_id, text, emoji) VALUES (?, ?, ?, ?)",
        ))
        .bind(message_id)
        .bind(index as i64 + 1)
        .bind(answer.poll_media.text.trim())
        .bind(emoji)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// Attaches `poll` to each message that has one, with the counts and the caller's own votes.
pub async fn hydrate(
    state: &AppState,
    me: &str,
    messages: &mut [Message],
) -> Result<(), sqlx::Error> {
    for message in messages.iter_mut() {
        message.poll = load(state, me, &message.id).await?;
    }
    Ok(())
}

async fn load(state: &AppState, me: &str, message_id: &str) -> Result<Option<Poll>, sqlx::Error> {
    let row: Option<(String, Option<String>, i64, i64)> = sqlx::query_as(&crate::db::sql(
        "SELECT question, expires_at, multiselect, finalized FROM polls WHERE message_id = ?",
    ))
    .bind(message_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((question, expires_at, multiselect, finalized)) = row else {
        return Ok(None);
    };
    let (multiselect, finalized) = (multiselect != 0, finalized != 0);
    let answers: Vec<(i64, String, Option<String>)> = sqlx::query_as(&crate::db::sql(
        "SELECT answer_id, text, emoji FROM poll_answers WHERE message_id = ? ORDER BY answer_id",
    ))
    .bind(message_id)
    .fetch_all(&state.db)
    .await?;
    let counts: Vec<(i64, i64, i64)> = sqlx::query_as(&crate::db::sql(
        "SELECT answer_id, COUNT(*), MAX(CASE WHEN user_id = ? THEN 1 ELSE 0 END) \
         FROM poll_votes WHERE message_id = ? GROUP BY answer_id",
    ))
    .bind(me)
    .bind(message_id)
    .fetch_all(&state.db)
    .await?;
    let expired = expires_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t < chrono::Utc::now());
    let answer_counts = answers
        .iter()
        .map(|(id, _, _)| {
            let found = counts.iter().find(|c| c.0 == *id);
            PollCount {
                id: *id as u32,
                count: found.map_or(0, |c| c.1 as u32),
                me_voted: found.is_some_and(|c| c.2 == 1),
            }
        })
        .collect();
    Ok(Some(Poll {
        question: PollText { text: question },
        answers: answers
            .into_iter()
            .map(|(id, text, emoji)| PollAnswer {
                answer_id: id as u32,
                poll_media: PollMedia {
                    text,
                    emoji: emoji.map(|key| reactions::emoji_json(&key)),
                },
            })
            .collect(),
        expiry: expires_at,
        allow_multiselect: multiselect,
        layout_type: 1,
        results: PollResults {
            is_finalized: finalized || expired,
            answer_counts,
        },
    }))
}

async fn publish_to_channel(
    state: &AppState,
    channel: &chat::ChannelRow,
    event: &'static str,
    data: serde_json::Value,
) -> ApiResult<()> {
    let targets = chat::channel_audience(state, channel).await.map_err(db)?;
    state.publish(targets, event, data);
    Ok(())
}

/// Replaces the caller's votes on a poll. An empty `answer_ids` clears them.
async fn vote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
    Json(body): Json<PollVote>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        let message = chat::message_by_id(&state, &message_id).await?;
        if message.channel_id != channel_id {
            return Err(chat::missing_message());
        }
        let poll: Option<(i64, i64, Option<String>)> = sqlx::query_as(
            &crate::db::sql("SELECT multiselect, finalized, expires_at FROM polls WHERE message_id = ?"),
        )
        .bind(&message_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db)?;
        let Some((multiselect, finalized, expires_at)) = poll else {
            return Err(chat::missing_message());
        };
        let (multiselect, finalized) = (multiselect != 0, finalized != 0);
        let expired = expires_at
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| t < chrono::Utc::now());
        if finalized || expired {
            return Err(api_error(StatusCode::BAD_REQUEST, 50035, "this poll has ended"));
        }

        let wanted: BTreeSet<u32> = body.answer_ids.iter().copied().collect();
        if wanted.len() != body.answer_ids.len()
            || (!multiselect && wanted.len() > 1)
            || wanted.len() > MAX_ANSWERS
        {
            return Err(invalid("invalid poll answers"));
        }
        let valid: HashSet<i64> = sqlx::query_scalar(&crate::db::sql("SELECT answer_id FROM poll_answers WHERE message_id = ?"))
            .bind(&message_id)
            .fetch_all(&state.db)
            .await
            .map_err(db)?
            .into_iter()
            .collect();
        if wanted.iter().any(|id| !valid.contains(&i64::from(*id))) {
            return Err(invalid("unknown poll answer"));
        }

        let previous: Vec<i64> = sqlx::query_scalar(
            &crate::db::sql("SELECT answer_id FROM poll_votes WHERE message_id = ? AND user_id = ?"),
        )
        .bind(&message_id)
        .bind(&user.id)
        .fetch_all(&state.db)
        .await
        .map_err(db)?;
        let previous: BTreeSet<u32> = previous.into_iter().map(|id| id as u32).collect();
        let removed: Vec<u32> = previous.difference(&wanted).copied().collect();
        let added: Vec<u32> = wanted.difference(&previous).copied().collect();

        let mut tx = state.db.begin().await.map_err(db)?;
        for answer in &removed {
            sqlx::query(&crate::db::sql("DELETE FROM poll_votes WHERE message_id = ? AND user_id = ? AND answer_id = ?"))
                .bind(&message_id)
                .bind(&user.id)
                .bind(i64::from(*answer))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        for answer in &added {
            sqlx::query(&crate::db::sql("INSERT INTO poll_votes (message_id, answer_id, user_id, created_at) VALUES (?, ?, ?, ?)"))
                .bind(&message_id)
                .bind(i64::from(*answer))
                .bind(&user.id)
                .bind(chat::now_iso())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;

        let guild_id = channel.guild_id.clone();
        for (event, answers) in [("MESSAGE_POLL_VOTE_REMOVE", &removed), ("MESSAGE_POLL_VOTE_ADD", &added)] {
            for answer in answers {
                publish_to_channel(&state, &channel, event, json!({
                    "user_id": user.id, "channel_id": channel_id, "message_id": message_id,
                    "guild_id": guild_id, "answer_id": answer,
                }))
                .await?;
            }
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

/// Ends a poll now. Only the author of the poll message may do this.
async fn expire(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, message_id)): Path<(String, String)>,
) -> Response {
    let result: ApiResult<Response> = async {
        let user = chat::authed(&state, &headers).await?;
        let channel = chat::channel_row(&state, &channel_id).await?;
        chat::require_channel(&state, &channel, &user.id).await?;
        let message = chat::message_by_id(&state, &message_id).await?;
        if message.channel_id != channel_id {
            return Err(chat::missing_message());
        }
        if message.author_id != user.id {
            return Err(chat::no_access());
        }
        let updated = sqlx::query(&crate::db::sql(
            "UPDATE polls SET finalized = 1 WHERE message_id = ?",
        ))
        .bind(&message_id)
        .execute(&state.db)
        .await
        .map_err(db)?;
        if updated.rows_affected() == 0 {
            return Err(chat::missing_message());
        }
        let mut out = vec![chat::message_json(
            chat::message_by_id(&state, &message_id).await?,
        )];
        uploads::hydrate(&state, &mut out).await.map_err(db)?;
        crate::reactions::hydrate(&state, &user.id, &mut out)
            .await
            .map_err(db)?;
        hydrate(&state, &user.id, &mut out).await.map_err(db)?;
        let message = out.remove(0);
        publish_to_channel(
            &state,
            &channel,
            "MESSAGE_UPDATE",
            serde_json::to_value(&message).unwrap_or_default(),
        )
        .await?;
        Ok(axum::Json(message).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e)
}

//! Permission bits and the rules that combine them: base permissions from the guild's
//! `@everyone` role and the member's roles, then channel overwrites in Discord's order.
//! The guild owner and anyone with ADMINISTRATOR hold every permission.
use axum::http::StatusCode;
use axum::response::Response;

use crate::{api::api_error, chat::ApiResult, state::AppState};

pub const CREATE_INSTANT_INVITE: u64 = 1 << 0;
pub const KICK_MEMBERS: u64 = 1 << 1;
pub const BAN_MEMBERS: u64 = 1 << 2;
pub const ADMINISTRATOR: u64 = 1 << 3;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
pub const MANAGE_GUILD: u64 = 1 << 5;
pub const ADD_REACTIONS: u64 = 1 << 6;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
pub const CONNECT: u64 = 1 << 20;
pub const SPEAK: u64 = 1 << 21;
pub const CHANGE_NICKNAME: u64 = 1 << 26;
pub const MANAGE_NICKNAMES: u64 = 1 << 27;
pub const MANAGE_ROLES: u64 = 1 << 28;

/// What a new guild's `@everyone` role grants.
pub const DEFAULT_EVERYONE: u64 = VIEW_CHANNEL
    | SEND_MESSAGES
    | READ_MESSAGE_HISTORY
    | ADD_REACTIONS
    | CREATE_INSTANT_INVITE
    | CONNECT
    | SPEAK
    | CHANGE_NICKNAME;

pub const ALL: u64 = u64::MAX;

/// Overwrite target types, as Discord numbers them.
pub const TARGET_ROLE: u8 = 0;
pub const TARGET_MEMBER: u8 = 1;

pub fn missing() -> Response {
    api_error(StatusCode::FORBIDDEN, 50013, "Missing Permissions")
}

/// Guild-wide permissions: owner and ADMINISTRATOR get everything, otherwise the OR of the
/// `@everyone` role and the member's roles.
pub async fn guild_permissions(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<u64> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM guilds WHERE id = ?")
        .bind(guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(crate::chat::db)?;
    let Some((owner_id,)) = owner else {
        return Ok(0);
    };
    if owner_id == user_id {
        return Ok(ALL);
    }
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT permissions FROM roles WHERE guild_id = ? AND (id = ? OR id IN \
         (SELECT role_id FROM member_roles WHERE guild_id = ? AND user_id = ?))",
    )
    .bind(guild_id)
    .bind(guild_id)
    .bind(guild_id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(crate::chat::db)?;
    let base = rows
        .iter()
        .filter_map(|(bits,)| bits.parse::<u64>().ok())
        .fold(0u64, |acc, bits| acc | bits);
    Ok(if base & ADMINISTRATOR != 0 { ALL } else { base })
}

/// Channel permissions: the guild base, then `@everyone`, then role overwrites (allows and
/// denies combined across roles), then the member's own overwrite.
pub async fn channel_permissions(
    state: &AppState,
    guild_id: &str,
    channel_id: &str,
    user_id: &str,
) -> ApiResult<u64> {
    let base = guild_permissions(state, guild_id, user_id).await?;
    if base & ADMINISTRATOR != 0 {
        return Ok(ALL);
    }
    let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT target_id, target_type, allow, deny FROM overwrites WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_all(&state.db)
    .await
    .map_err(crate::chat::db)?;
    let my_roles: Vec<(String,)> =
        sqlx::query_as("SELECT role_id FROM member_roles WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(user_id)
            .fetch_all(&state.db)
            .await
            .map_err(crate::chat::db)?;
    let my_roles: Vec<String> = my_roles.into_iter().map(|(id,)| id).collect();
    let parse = |s: &str| s.parse::<u64>().unwrap_or(0);

    let mut perms = base;
    // @everyone (the role whose id is the guild id).
    for (target, kind, allow, deny) in &rows {
        if *kind == TARGET_ROLE as i64 && target == guild_id {
            perms = (perms & !parse(deny)) | parse(allow);
        }
    }
    let (mut role_allow, mut role_deny) = (0u64, 0u64);
    for (target, kind, allow, deny) in &rows {
        if *kind == TARGET_ROLE as i64 && target != guild_id && my_roles.contains(target) {
            role_allow |= parse(allow);
            role_deny |= parse(deny);
        }
    }
    perms = (perms & !role_deny) | role_allow;
    for (target, kind, allow, deny) in &rows {
        if *kind == TARGET_MEMBER as i64 && target == user_id {
            perms = (perms & !parse(deny)) | parse(allow);
        }
    }
    Ok(perms)
}

/// Fails with 403 unless `user_id` holds `bit` in the guild.
pub async fn require_guild(
    state: &AppState,
    guild_id: &str,
    user_id: &str,
    bit: u64,
) -> ApiResult<()> {
    if guild_permissions(state, guild_id, user_id).await? & bit == bit {
        Ok(())
    } else {
        Err(missing())
    }
}

/// Fails with 403 unless `user_id` holds `bit` in the channel.
pub async fn require_channel(
    state: &AppState,
    guild_id: &str,
    channel_id: &str,
    user_id: &str,
    bit: u64,
) -> ApiResult<()> {
    if channel_permissions(state, guild_id, channel_id, user_id).await? & bit == bit {
        Ok(())
    } else {
        Err(missing())
    }
}

/// Checks `bit` on a channel row. Direct messages have no guild permissions, so they pass.
pub async fn require_channel_row(
    state: &AppState,
    channel: &crate::chat::ChannelRow,
    user_id: &str,
    bit: u64,
) -> ApiResult<()> {
    match &channel.guild_id {
        Some(guild_id) => require_channel(state, guild_id, &channel.id, user_id, bit).await,
        None => Ok(()),
    }
}

/// Whether `user_id` owns the guild. Owners pass every hierarchy check.
pub async fn is_owner(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<bool> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM guilds WHERE id = ?")
        .bind(guild_id)
        .fetch_optional(&state.db)
        .await
        .map_err(crate::chat::db)?;
    Ok(owner.is_some_and(|(id,)| id == user_id))
}

/// The highest role position a member holds. `@everyone` counts as position 0. The owner
/// counts as the top of the hierarchy.
pub async fn top_position(state: &AppState, guild_id: &str, user_id: &str) -> ApiResult<i64> {
    if is_owner(state, guild_id, user_id).await? {
        return Ok(i64::MAX);
    }
    let top: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT MAX(position) FROM roles WHERE guild_id = ? AND (id = ? OR id IN \
         (SELECT role_id FROM member_roles WHERE guild_id = ? AND user_id = ?))",
    )
    .bind(guild_id)
    .bind(guild_id)
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(crate::chat::db)?;
    Ok(top.and_then(|(max,)| max).unwrap_or(0))
}

/// The actor may act on a role only when the role sits below their own top role.
pub async fn require_above_role(
    state: &AppState,
    guild_id: &str,
    user_id: &str,
    role_id: &str,
) -> ApiResult<()> {
    let top = top_position(state, guild_id, user_id).await?;
    if top == i64::MAX {
        return Ok(());
    }
    let position: Option<(i64,)> =
        sqlx::query_as("SELECT position FROM roles WHERE id = ? AND guild_id = ?")
            .bind(role_id)
            .bind(guild_id)
            .fetch_optional(&state.db)
            .await
            .map_err(crate::chat::db)?;
    match position {
        Some((position,)) if position < top => Ok(()),
        _ => Err(missing()),
    }
}

/// The actor may act on a member only when the member's top role sits below their own.
/// Acting on yourself is always allowed here; callers decide whether the action makes sense.
pub async fn require_above_member(
    state: &AppState,
    guild_id: &str,
    actor: &str,
    target: &str,
) -> ApiResult<()> {
    if actor == target {
        return Ok(());
    }
    let top = top_position(state, guild_id, actor).await?;
    if top == i64::MAX {
        return Ok(());
    }
    let target_top = top_position(state, guild_id, target).await?;
    if target_top < top {
        Ok(())
    } else {
        Err(missing())
    }
}

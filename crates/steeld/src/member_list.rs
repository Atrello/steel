//! Sidebar member list. Flint subscribes to a guild with op 37 and names the ranges it wants per
//! channel, `channels: {channel_id: [[start, end]]}`. Steel answers on the same socket with
//! `GUILD_MEMBER_LIST_UPDATE` SYNC ops for those ranges. The list is one `everyone` list sorted by
//! username. Presence is not in the list yet, so every member sits in the `offline` group.

use flint_protocol::GatewayFrame;
use serde_json::{json, Value};

use crate::state::AppState;

pub const EVERYONE_LIST: &str = "everyone";
/// Largest range one subscription may ask for, as in Discord's member list pages.
const MAX_RANGE_SPAN: usize = 100;

/// Frames to send back for one op 37 payload. Empty when the payload only subscribes the guild.
pub async fn answer(state: &AppState, user_id: &str, data: &Value) -> Vec<GatewayFrame> {
    let mut frames = Vec::new();
    let Some(subscriptions) = data.get("subscriptions").and_then(Value::as_object) else {
        return frames;
    };
    for (guild_id, sub) in subscriptions {
        let Some(channels) = sub.get("channels").and_then(Value::as_object) else {
            continue;
        };
        if channels.is_empty() || !is_member(state, guild_id, user_id).await {
            continue;
        }
        let members = match load_members(state, guild_id).await {
            Ok(members) => members,
            Err(_) => continue,
        };
        for (channel_id, ranges) in channels {
            if !in_guild(state, channel_id, guild_id).await {
                continue;
            }
            let ranges: Vec<[usize; 2]> = ranges
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let pair = r.as_array()?;
                    let start = pair.first()?.as_u64()? as usize;
                    let end = pair.get(1)?.as_u64()? as usize;
                    (start <= end && end - start < MAX_RANGE_SPAN).then_some([start, end])
                })
                .collect();
            if ranges.is_empty() {
                continue;
            }
            frames.push(sync_frame(guild_id, &members, &ranges));
        }
    }
    frames
}

/// One `GUILD_MEMBER_LIST_UPDATE` with a SYNC op per requested range.
fn sync_frame(
    guild_id: &str,
    members: &[(String, String, String, Option<String>)],
    ranges: &[[usize; 2]],
) -> GatewayFrame {
    let total = members.len();
    let ops: Vec<Value> = ranges
        .iter()
        .map(|[start, end]| {
            let items: Vec<Value> = members
                .iter()
                .enumerate()
                .filter(|(i, _)| i >= start && i <= end)
                .map(|(_, (id, username, joined_at, nick))| {
                    json!({"member": {
                        "user": {"id": id, "username": username, "avatar": null, "global_name": null, "bot": false},
                        "nick": nick, "joined_at": joined_at, "roles": [], "deaf": false, "mute": false,
                    }})
                })
                .collect();
            json!({"op": "SYNC", "range": [start, end], "items": items})
        })
        .collect();
    GatewayFrame {
        op: flint_protocol::op::DISPATCH,
        d: json!({
            "guild_id": guild_id,
            "id": EVERYONE_LIST,
            "member_count": total,
            "groups": [{"id": "offline", "count": total}],
            "ops": ops,
        }),
        s: None,
        t: Some("GUILD_MEMBER_LIST_UPDATE".to_string()),
    }
}

async fn load_members(
    state: &AppState,
    guild_id: &str,
) -> Result<Vec<(String, String, String, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.username, m.joined_at, m.nick FROM guild_members m JOIN users u ON u.id = m.user_id \
         WHERE m.guild_id = ? ORDER BY LOWER(u.username), u.id",
    )
    .bind(guild_id)
    .fetch_all(&state.db)
    .await
}

async fn is_member(state: &AppState, guild_id: &str, user_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM guild_members WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn in_guild(state: &AppState, channel_id: &str, guild_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM channels WHERE id = ? AND guild_id = ?")
        .bind(channel_id)
        .bind(guild_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .is_some()
}

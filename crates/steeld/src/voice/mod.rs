//! Voice: the Discord-shaped voice gateway, call membership and the UDP media relay.
//!
//! Steel is a selective forwarding server. It never decrypts media. Each call has one transport
//! key, which the server hands to every participant, and it forwards each encrypted RTP packet to
//! the other participants of the same call. DAVE (MLS) end-to-end encryption rides on top of the
//! transport and is handled by the clients.

pub mod signal;
pub mod udp;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use flint_protocol::User;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Notify};

pub use signal::run_ws;

/// The only transport mode Steel offers. It matches the mode Flint's voice crate requires.
pub const MODE: &str = "aead_xchacha20_poly1305_rtpsize";

/// Capacity of the per-call signaling broadcast.
const EVENT_BUFFER: usize = 256;

#[derive(Clone, Debug)]
pub struct Config {
    /// UDP port for media and IP discovery.
    pub port: u16,
    /// Address advertised to clients in the voice Ready message.
    pub advertise_ip: String,
    /// Host and port of the voice WebSocket, sent in VOICE_SERVER_UPDATE (for example `steel.example.com:443`).
    pub endpoint: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = std::env::var("STEEL_VOICE_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50000);
        Self {
            port,
            advertise_ip: std::env::var("STEEL_VOICE_IP").unwrap_or_else(|_| "127.0.0.1".into()),
            endpoint: std::env::var("STEEL_VOICE_ENDPOINT")
                .unwrap_or_else(|_| format!("127.0.0.1:{port}")),
        }
    }
}

/// One participant in a call.
#[derive(Clone)]
pub struct Member {
    pub channel_id: String,
    pub user_id: String,
    pub session_id: String,
    pub token: String,
    pub ssrc: u32,
    /// Set by IP discovery. Media is relayed only to participants with an address.
    pub addr: Option<SocketAddr>,
    /// Fires when the membership ends, so the participant's signaling socket closes.
    pub ended: Arc<Notify>,
}

/// A signaling event for everyone in a call except `exclude`.
#[derive(Clone, Debug)]
pub struct CallEvent {
    pub exclude: String,
    pub text: String,
}

struct Call {
    key: [u8; 32],
    members: HashMap<String, Member>,
    events: broadcast::Sender<CallEvent>,
}

#[derive(Default)]
struct Inner {
    calls: HashMap<String, Call>,
    /// user id -> channel id, for the user's one active call.
    by_user: HashMap<String, String>,
    by_ssrc: HashMap<u32, (String, String)>,
    by_addr: HashMap<SocketAddr, (String, String)>,
}

/// What a voice WebSocket receives after it identifies.
pub struct Attached {
    pub member: Member,
    pub key: [u8; 32],
    pub events: broadcast::Receiver<CallEvent>,
    pub roster: Vec<String>,
}

pub struct Registry {
    inner: Mutex<Inner>,
    pub config: Config,
}

impl Registry {
    pub fn new(config: Config) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            config,
        }
    }

    /// Join `channel_id`, leaving any other call first. Returns the new membership and the channel
    /// the user left, if any.
    pub fn join(
        &self,
        channel_id: &str,
        user_id: &str,
        session_id: &str,
    ) -> (Member, Option<String>) {
        let mut inner = self.inner.lock().expect("voice registry poisoned");
        let previous = leave_locked(&mut inner, user_id);

        let ssrc = fresh_ssrc(&inner);
        let token = crate::auth::new_token();
        let member = Member {
            channel_id: channel_id.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            token,
            ssrc,
            addr: None,
            ended: Arc::new(Notify::new()),
        };

        let call = inner.calls.entry(channel_id.to_string()).or_insert_with(|| {
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            Call {
                key,
                members: HashMap::new(),
                events: broadcast::channel(EVENT_BUFFER).0,
            }
        });
        call.members.insert(user_id.to_string(), member.clone());
        inner
            .by_user
            .insert(user_id.to_string(), channel_id.to_string());
        inner
            .by_ssrc
            .insert(ssrc, (channel_id.to_string(), user_id.to_string()));
        (member, previous)
    }

    /// End the user's call membership. Returns the channel left, if the user was in one.
    pub fn leave(&self, user_id: &str) -> Option<String> {
        let mut inner = self.inner.lock().expect("voice registry poisoned");
        leave_locked(&mut inner, user_id)
    }

    /// Leave only if `token` is still the user's live membership, so a stale socket cannot end a newer call.
    pub fn leave_if_current(&self, channel_id: &str, user_id: &str, token: &str) -> bool {
        let mut inner = self.inner.lock().expect("voice registry poisoned");
        let current = inner
            .calls
            .get(channel_id)
            .and_then(|c| c.members.get(user_id))
            .is_some_and(|m| m.token == token);
        if current {
            leave_locked(&mut inner, user_id);
        }
        current
    }

    /// Validate a voice Identify. Returns the membership and the call's state for this socket.
    pub fn attach(&self, token: &str, user_id: &str, session_id: &str) -> Option<Attached> {
        let inner = self.inner.lock().expect("voice registry poisoned");
        let channel_id = inner.by_user.get(user_id)?.clone();
        let call = inner.calls.get(&channel_id)?;
        let member = call.members.get(user_id)?;
        if member.token != token || member.session_id != session_id {
            return None;
        }
        let member = member.clone();
        let key = call.key;
        let events = call.events.subscribe();
        let mut roster: Vec<String> = call
            .members
            .keys()
            .filter(|id| id.as_str() != user_id)
            .cloned()
            .collect();
        roster.sort();
        let _ = call.events.send(CallEvent {
            exclude: user_id.to_string(),
            text: json!({ "op": 11, "d": { "user_ids": [user_id] } }).to_string(),
        });
        Some(Attached {
            member,
            key,
            events,
            roster,
        })
    }

    /// Send an event to everyone in `channel_id` except `exclude`.
    pub fn broadcast(&self, channel_id: &str, exclude: &str, text: String) {
        let inner = self.inner.lock().expect("voice registry poisoned");
        if let Some(call) = inner.calls.get(channel_id) {
            let _ = call.events.send(CallEvent {
                exclude: exclude.to_string(),
                text,
            });
        }
    }

    /// Record the address IP discovery saw for `ssrc`. Returns the membership if the SSRC is live.
    pub fn bind_addr(&self, ssrc: u32, addr: SocketAddr) -> Option<Member> {
        let mut inner = self.inner.lock().expect("voice registry poisoned");
        let (channel_id, user_id) = inner.by_ssrc.get(&ssrc)?.clone();
        let member = inner.calls.get_mut(&channel_id)?.members.get_mut(&user_id)?;
        let old = member.addr.replace(addr);
        let member = member.clone();
        if let Some(old) = old {
            inner.by_addr.remove(&old);
        }
        inner.by_addr.insert(addr, (channel_id, user_id));
        Some(member)
    }

    /// Addresses of the other participants in the call `src` belongs to.
    pub fn route(&self, src: SocketAddr) -> Option<Vec<SocketAddr>> {
        let inner = self.inner.lock().expect("voice registry poisoned");
        let (channel_id, user_id) = inner.by_addr.get(&src)?;
        let call = inner.calls.get(channel_id)?;
        Some(
            call.members
                .values()
                .filter(|m| &m.user_id != user_id)
                .filter_map(|m| m.addr)
                .collect(),
        )
    }

    /// The channel a user is currently in.
    pub fn channel_of(&self, user_id: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("voice registry poisoned")
            .by_user
            .get(user_id)
            .cloned()
    }
}

fn leave_locked(inner: &mut Inner, user_id: &str) -> Option<String> {
    let channel_id = inner.by_user.remove(user_id)?;
    let call = inner.calls.get_mut(&channel_id)?;
    let member = call.members.remove(user_id)?;
    inner.by_ssrc.remove(&member.ssrc);
    if let Some(addr) = member.addr {
        inner.by_addr.remove(&addr);
    }
    member.ended.notify_waiters();
    let _ = call.events.send(CallEvent {
        exclude: user_id.to_string(),
        text: json!({ "op": 13, "d": { "user_id": user_id } }).to_string(),
    });
    if call.members.is_empty() {
        inner.calls.remove(&channel_id);
    }
    Some(channel_id)
}

fn fresh_ssrc(inner: &Inner) -> u32 {
    loop {
        let ssrc = rand::thread_rng().next_u32();
        if ssrc != 0 && !inner.by_ssrc.contains_key(&ssrc) {
            return ssrc;
        }
    }
}

/// Handle a client's voice state update (gateway opcode 4).
/// `channel_id` null leaves the call. Otherwise the server checks that the user can see the channel,
/// then tells the channel's audience and sends the user a server endpoint and token.
pub async fn gateway_state(state: &crate::state::AppState, user: &User, session_id: &str, data: Value) {
    let channel_id = id_field(&data, "channel_id");
    let guild_hint = id_field(&data, "guild_id");
    let self_mute = data["self_mute"].as_bool().unwrap_or(false);
    let self_deaf = data["self_deaf"].as_bool().unwrap_or(false);

    let Some(channel_id) = channel_id else {
        if let Some(prev) = state.voice.leave(&user.id) {
            announce_state(state, &prev, &user.id, session_id, None, None, false, false).await;
        }
        return;
    };

    let Ok(row) = crate::chat::channel_row(state, &channel_id).await else {
        return;
    };
    let audience = match crate::chat::channel_audience(state, &row).await {
        Ok(a) => a,
        Err(_) => return,
    };
    if !audience.contains(&user.id) {
        return;
    }
    let guild_id = row.guild_id.clone().or(guild_hint);

    let (member, previous) = state
        .voice
        .join(&channel_id, &user.id, session_id);
    if let Some(prev) = previous.filter(|p| p != &channel_id) {
        announce_state(state, &prev, &user.id, session_id, None, None, false, false).await;
    }
    announce_state(
        state,
        &channel_id,
        &user.id,
        session_id,
        guild_id.clone(),
        Some(audience),
        self_mute,
        self_deaf,
    )
    .await;

    state.publish(
        vec![user.id.clone()],
        "VOICE_SERVER_UPDATE",
        json!({
            "token": member.token,
            "guild_id": guild_id,
            "channel_id": channel_id,
            "endpoint": state.voice.config.endpoint,
        }),
    );
}

/// Publish a VOICE_STATE_UPDATE for `user_id` in `channel_id` (`None` audience means the channel is
/// already gone, so the update goes to nobody extra). Passing no audience looks the audience up.
#[allow(clippy::too_many_arguments)]
async fn announce_state(
    state: &crate::state::AppState,
    channel_id: &str,
    user_id: &str,
    session_id: &str,
    guild_id: Option<String>,
    audience: Option<Vec<String>>,
    self_mute: bool,
    self_deaf: bool,
) {
    let audience = match audience {
        Some(a) => a,
        None => match crate::chat::channel_row(state, channel_id).await {
            Ok(row) => crate::chat::channel_audience(state, &row).await.unwrap_or_default(),
            Err(_) => vec![user_id.to_string()],
        },
    };
    let left = !state.voice.channel_of(user_id).is_some_and(|c| c == channel_id);
    let channel_value = if left { Value::Null } else { json!(channel_id) };
    state.publish(
        audience,
        "VOICE_STATE_UPDATE",
        json!({
            "guild_id": guild_id,
            "channel_id": channel_value,
            "user_id": user_id,
            "session_id": session_id,
            "self_mute": self_mute,
            "self_deaf": self_deaf,
            "self_video": false,
            "suppress": false,
        }),
    );
}

/// Read a snowflake that clients may send as a string or a number.
fn id_field(data: &Value, key: &str) -> Option<String> {
    match &data[key] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

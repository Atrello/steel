//! Wire types shared between `steeld` and Flint.
//!
//! The format is Discord-shaped JSON so the existing client UI can talk to Steel.
//! Snowflake IDs are serialized as strings on the wire.

use serde::{Deserialize, Serialize};

/// Gateway opcodes used by Phase 1.
pub mod op {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const PRESENCE_UPDATE: u8 = 3;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
    /// Guild subscriptions: member list ranges and typing (Flint sends this as op 37).
    pub const GUILD_SUBSCRIBE: u8 = 37;
}

/// A gateway frame in either direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayFrame {
    pub op: u8,
    #[serde(default)]
    pub d: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

impl GatewayFrame {
    pub fn hello(heartbeat_interval_ms: u64) -> Self {
        Self {
            op: op::HELLO,
            d: serde_json::json!({ "heartbeat_interval": heartbeat_interval_ms }),
            s: None,
            t: None,
        }
    }

    pub fn dispatch(seq: u64, event: &str, data: serde_json::Value) -> Self {
        Self {
            op: op::DISPATCH,
            d: data,
            s: Some(seq),
            t: Some(event.to_string()),
        }
    }

    pub fn ack() -> Self {
        Self {
            op: op::HEARTBEAT_ACK,
            d: serde_json::Value::Null,
            s: None,
            t: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub login: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub user: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifyData {
    pub token: String,
}

/// Error body in the Discord style `{ "code": n, "message": "..." }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: u32,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_shape() {
        let v = serde_json::to_value(GatewayFrame::hello(41250)).unwrap();
        assert_eq!(v["op"], 10);
        assert_eq!(v["d"]["heartbeat_interval"], 41250);
    }
}

// ---------------------------------------------------------------------------
// Guilds, channels, messages, invites
// ---------------------------------------------------------------------------

/// Channel types. Only text channels exist in this chunk.
pub mod channel_type {
    pub const GUILD_TEXT: u8 = 0;
    pub const DM: u8 = 1;
    pub const GROUP_DM: u8 = 3;
    pub const GUILD_VOICE: u8 = 2;
    pub const GUILD_CATEGORY: u8 = 4;
}

/// Relationship kinds, as in Discord's relationship API.
pub mod relationship_type {
    pub const FRIEND: u8 = 1;
    pub const BLOCKED: u8 = 2;
    pub const INCOMING_REQUEST: u8 = 3;
    pub const OUTGOING_REQUEST: u8 = 4;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub topic: Option<String>,
    /// Category this channel sits under; null for categories and uncategorized channels.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Permission overwrites. Steel has no per-channel overwrites yet, so this stays empty;
    /// clients read it to copy a category's overwrites onto a new child channel.
    #[serde(default)]
    pub permission_overwrites: Vec<serde_json::Value>,
    /// Participants of a DM or group DM. Empty for guild channels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<User>,
    /// Set on threads (types 10 to 12) only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_metadata: Option<ThreadMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadMetadata {
    pub archived: bool,
    pub auto_archive_duration: u32,
    /// RFC 3339 time of the last archive change, or thread creation.
    pub archive_timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub user: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDm {
    #[serde(default)]
    pub recipient_id: Option<String>,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendFriendRequest {
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRelationship {
    #[serde(rename = "type", default)]
    pub kind: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// Roles, lowest position first. The `@everyone` role has the guild's id.
    #[serde(default)]
    pub roles: Vec<serde_json::Value>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageReference {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub guild_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    #[serde(default)]
    pub content_type: Option<String>,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReactionEmoji {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: ReactionEmoji,
    pub count: u32,
    pub me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub author: User,
    pub content: String,
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_reference: Option<MessageReference>,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// Message flags. 64 marks an ephemeral message, visible only to the invoking user.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flags: u32,
    /// Bot message components (action rows of buttons or menus), passed through as sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<Poll>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A poll attached to a message: `poll` on the message object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Poll {
    pub question: PollText,
    pub answers: Vec<PollAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    pub allow_multiselect: bool,
    pub layout_type: u8,
    pub results: PollResults,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollText {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollAnswer {
    pub answer_id: u32,
    pub poll_media: PollMedia,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollMedia {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<ReactionEmoji>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollResults {
    pub is_finalized: bool,
    pub answer_counts: Vec<PollCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollCount {
    pub id: u32,
    pub count: u32,
    pub me_voted: bool,
}

/// Poll in a create-message request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePoll {
    pub question: PollText,
    pub answers: Vec<CreatePollAnswer>,
    /// Hours the poll stays open.
    pub duration: u16,
    #[serde(default)]
    pub allow_multiselect: bool,
    #[serde(default = "default_layout")]
    pub layout_type: u8,
}

fn default_layout() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePollAnswer {
    pub poll_media: PollMedia,
}

/// Body of `PUT /channels/{id}/polls/{id}/answers/@me`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollVote {
    pub answer_ids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGuild {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChannel {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: Option<u8>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub permission_overwrites: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessage {
    /// Poll and sticker-only messages may omit content, as Discord clients send them.
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    /// Files staged with `POST /channels/{id}/attachments` and uploaded to their `upload_url`.
    #[serde(default)]
    pub attachments: Vec<UploadRef>,
    /// Components, accepted from bot accounts only.
    #[serde(default)]
    pub components: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<CreatePoll>,
}

/// Reference to an uploaded file, as sent in a message's `attachments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRef {
    #[serde(default)]
    pub id: serde_json::Value,
    pub filename: String,
    pub uploaded_filename: String,
}

/// Request for upload slots: `POST /channels/{id}/attachments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageFiles {
    pub files: Vec<StageFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageFile {
    pub id: serde_json::Value,
    pub filename: String,
    pub file_size: u64,
    #[serde(default)]
    pub is_clip: bool,
    #[serde(default)]
    pub original_content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedUpload {
    pub id: serde_json::Value,
    pub upload_url: String,
    pub upload_filename: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedUploads {
    pub attachments: Vec<StagedUpload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditMessage {
    pub content: String,
}

/// Body of `POST /applications` (bot account creation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApplication {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub code: String,
    pub guild: Guild,
    pub channel: Channel,
}

/// Presence status values.
pub mod status {
    pub const ONLINE: &str = "online";
    pub const IDLE: &str = "idle";
    pub const DND: &str = "dnd";
    pub const INVISIBLE: &str = "invisible";
    pub const OFFLINE: &str = "offline";
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadState {
    pub id: String,
    pub last_message_id: Option<String>,
    pub mention_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceUpdate {
    pub status: String,
    #[serde(default)]
    pub since: Option<u64>,
    #[serde(default)]
    pub afk: bool,
    #[serde(default)]
    pub activities: Vec<serde_json::Value>,
}

/// Body of `POST /channels/{id}/threads` and `POST /channels/{id}/messages/{id}/threads`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThread {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: Option<u8>,
    #[serde(default)]
    pub auto_archive_duration: Option<u32>,
    /// Forum posts: the starter message. Required in forums, ignored elsewhere.
    #[serde(default)]
    pub message: Option<StarterMessage>,
}

/// Starter message of a forum post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarterMessage {
    #[serde(default)]
    pub content: String,
}

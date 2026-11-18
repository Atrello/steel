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
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
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

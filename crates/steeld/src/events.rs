//! Server-side event bus. REST handlers publish a `Dispatch`; each gateway connection
//! forwards the dispatches whose `targets` include its user.

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Dispatch {
    pub targets: Vec<String>,
    pub event: &'static str,
    pub data: Value,
}

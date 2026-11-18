use std::sync::Arc;

use tokio::sync::broadcast;

use crate::{db::Db, events::Dispatch, presence::PresenceTracker, session::SessionStore, voice};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub events: broadcast::Sender<Dispatch>,
    pub presence: Arc<PresenceTracker>,
    pub sessions: Arc<SessionStore>,
    pub voice: Arc<voice::Registry>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            db,
            events,
            presence: Arc::new(PresenceTracker::default()),
            sessions: Arc::new(SessionStore::default()),
            voice: Arc::new(voice::Registry::new(voice::Config::from_env())),
        }
    }

    /// Publish a dispatch to the connected users in `targets`. No-op when nobody is connected.
    pub fn publish(&self, targets: Vec<String>, event: &'static str, data: serde_json::Value) {
        let _ = self.events.send(crate::events::Dispatch {
            targets,
            event,
            data,
        });
    }
}

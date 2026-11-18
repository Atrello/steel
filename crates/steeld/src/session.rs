//! Gateway sessions. A session outlives its socket for `RESUME_WINDOW` so a client can resume and
//! receive the events it missed. Each session has a relay task that copies the events addressed to
//! its user into a bounded buffer, numbered with sequence numbers.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use flint_protocol::GatewayFrame;
use tokio::sync::{broadcast, watch, Notify};

use crate::events::Dispatch;

/// Events kept per session for resume.
pub const BUFFER_CAP: usize = 1024;
/// How long a detached session can be resumed.
pub const RESUME_WINDOW: Duration = Duration::from_secs(300);

pub struct Session {
    pub id: String,
    pub user_id: String,
    inner: Mutex<Inner>,
    /// Woken when new frames are buffered or the session ends.
    pub notify: Notify,
    dead_tx: watch::Sender<bool>,
}

struct Inner {
    /// Last sequence number assigned. READY is sequence 1.
    seq: u64,
    buf: VecDeque<GatewayFrame>,
    /// Incremented each time a socket attaches. Only the newest attachment is current.
    conn_gen: u64,
    attached: bool,
    detached_at: Option<Instant>,
    dead: bool,
}

impl Session {
    pub fn new(id: String, user_id: String) -> Arc<Self> {
        let (dead_tx, _) = watch::channel(false);
        Arc::new(Self {
            id,
            user_id,
            inner: Mutex::new(Inner {
                seq: 1,
                buf: VecDeque::new(),
                conn_gen: 0,
                attached: false,
                detached_at: None,
                dead: false,
            }),
            notify: Notify::new(),
            dead_tx,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Buffer a dispatch with the next sequence number.
    pub fn push(&self, event: &'static str, data: serde_json::Value) {
        {
            let mut inner = self.lock();
            if inner.dead {
                return;
            }
            inner.seq += 1;
            let seq = inner.seq;
            inner
                .buf
                .push_back(GatewayFrame::dispatch(seq, event, data));
            while inner.buf.len() > BUFFER_CAP {
                inner.buf.pop_front();
            }
        }
        self.notify.notify_waiters();
    }

    /// Frames with sequence numbers after `cursor`. `None` when the session has ended.
    pub fn frames_after(&self, cursor: u64) -> Option<Vec<GatewayFrame>> {
        let inner = self.lock();
        if inner.dead {
            return None;
        }
        Some(
            inner
                .buf
                .iter()
                .filter(|f| f.s.unwrap_or(0) > cursor)
                .cloned()
                .collect(),
        )
    }

    /// Whether a client that last saw `seq` can resume without missing events.
    pub fn can_resume_from(&self, seq: u64) -> bool {
        let inner = self.lock();
        if inner.dead || seq > inner.seq {
            return false;
        }
        match inner.buf.front() {
            // The oldest buffered frame must directly follow what the client saw.
            Some(front) => front.s.unwrap_or(0) <= seq + 1,
            None => true,
        }
    }

    /// Mark a socket as attached. Returns its generation; an older socket will stop on its next wake.
    pub fn attach(&self) -> u64 {
        let mut inner = self.lock();
        inner.conn_gen += 1;
        inner.attached = true;
        inner.detached_at = None;
        inner.conn_gen
    }

    pub fn is_current(&self, gen: u64) -> bool {
        let inner = self.lock();
        !inner.dead && inner.conn_gen == gen
    }

    /// Detach the socket with generation `gen`. Returns false when a newer socket already took over.
    pub fn detach(&self, gen: u64) -> bool {
        let mut inner = self.lock();
        if inner.conn_gen != gen {
            return false;
        }
        inner.attached = false;
        inner.detached_at = Some(Instant::now());
        true
    }

    /// True when nothing has attached since generation `gen` detached.
    pub fn still_detached_since(&self, gen: u64) -> bool {
        let inner = self.lock();
        !inner.attached && inner.conn_gen == gen && !inner.dead
    }

    pub fn is_dead(&self) -> bool {
        self.lock().dead
    }

    /// End the session. Its relay stops and its buffer is dropped.
    pub fn kill(&self) {
        {
            let mut inner = self.lock();
            inner.dead = true;
            inner.buf.clear();
        }
        let _ = self.dead_tx.send(true);
        self.notify.notify_waiters();
    }

    pub fn dead_watch(&self) -> watch::Receiver<bool> {
        self.dead_tx.subscribe()
    }
}

/// Copy the events addressed to this session's user into its buffer until the session ends.
pub fn spawn_relay(session: Arc<Session>, mut rx: broadcast::Receiver<Dispatch>) {
    let mut dead = session.dead_watch();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = dead.changed() => {
                    if changed.is_err() || *dead.borrow() {
                        break;
                    }
                }
                received = rx.recv() => match received {
                    Ok(dispatch) => {
                        if dispatch.targets.contains(&session.user_id) {
                            session.push(dispatch.event, dispatch.data);
                        }
                    }
                    // The relay fell behind and dropped events, so the session cannot be trusted to resume.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("session {} lagged by {n} events; ending it", session.id);
                        session.kill();
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
}

#[derive(Default)]
pub struct SessionStore {
    map: Mutex<HashMap<String, Arc<Session>>>,
}

impl SessionStore {
    pub fn insert(&self, session: Arc<Session>) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn remove(&self, id: &str) {
        if let Some(session) = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            session.kill();
        }
    }
}

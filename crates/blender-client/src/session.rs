//! The currently connected Blender instance.

use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use blender_protocol::{
    capabilities::{BlenderIdentity, Capabilities},
    envelope::{Envelope, SessionId},
    error::{BlenderError, ErrorCode},
    handshake::HelloAck,
};
use tokio::sync::mpsc;

/// A live connection to Blender.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub identity: Arc<BlenderIdentity>,
    pub capabilities: Arc<Capabilities>,
    /// Queue into the writer task. Bounded, so a Blender that stops reading
    /// applies backpressure instead of letting the queue grow without limit.
    pub outbound: mpsc::Sender<Envelope>,
    pub connected_at: Instant,
    /// Peer address, for diagnostics.
    pub peer: String,
}

impl Session {
    /// Hand a frame to the writer task.
    pub async fn send(&self, envelope: Envelope) -> Result<(), BlenderError> {
        self.outbound.send(envelope).await.map_err(|_| {
            BlenderError::new(
                ErrorCode::ConnectionLost,
                "The connection to Blender closed while the request was being queued.",
            )
        })
    }

    pub fn uptime(&self) -> Duration {
        self.connected_at.elapsed()
    }
}

/// Connection state as reported by `blender.status`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Status {
    pub connected: bool,
    /// Address the server is listening on for the add-on to dial into.
    pub listen_address: String,
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blender_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// True when Blender is running without a UI, which rules out viewport
    /// screenshots and any operator that needs a 3D view context.
    #[serde(default)]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_seconds: Option<u64>,
    #[serde(default)]
    pub pending_requests: usize,
    /// Why the last connection ended, when one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// The session slot, shared between the accept loop and every caller.
///
/// A plain `RwLock` is deliberate: the guard is never held across an `.await`,
/// only long enough to clone a handle out, so an async lock would add cost
/// without adding anything.
#[derive(Default)]
pub struct SessionSlot {
    current: RwLock<Option<Session>>,
    last_error: RwLock<Option<String>>,
}

impl SessionSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// The active session, if Blender is connected.
    pub fn get(&self) -> Option<Session> {
        self.read().clone()
    }

    /// The active session, or a `BLENDER_NOT_CONNECTED` error.
    pub fn require(&self) -> Result<Session, BlenderError> {
        self.get().ok_or_else(BlenderError::not_connected)
    }

    /// Install a new session, returning the one it replaced.
    ///
    /// Replacement is the normal path when Blender restarts: the old socket is
    /// often still half-open, and the new connection is authoritative.
    pub fn replace(&self, session: Session) -> Option<Session> {
        let mut guard = self.write();
        guard.replace(session)
    }

    /// Clear the session, but only if it is still the one that ended. Without
    /// the id check, a slow teardown of an old connection would wipe out the
    /// new one that had already taken its place.
    pub fn clear_if(&self, id: SessionId, reason: impl Into<String>) -> bool {
        let mut guard = self.write();
        let matches = guard.as_ref().is_some_and(|s| s.id == id);
        if matches {
            *guard = None;
            drop(guard);
            *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = Some(reason.into());
        }
        matches
    }

    pub fn record_error(&self, reason: impl Into<String>) {
        *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = Some(reason.into());
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Build the status report.
    pub fn status(&self, listen_address: &str, pending: usize) -> Status {
        let session = self.get();
        Status {
            connected: session.is_some(),
            listen_address: listen_address.to_string(),
            protocol_version: blender_protocol::PROTOCOL_VERSION,
            session_id: session.as_ref().map(|s| s.id.to_string()),
            blender_version: session
                .as_ref()
                .map(|s| s.identity.blender_version.to_string()),
            python_version: session.as_ref().map(|s| s.identity.python_version.clone()),
            addon_version: session.as_ref().map(|s| s.identity.addon_version.clone()),
            platform: session.as_ref().map(|s| s.identity.platform.clone()),
            background: session.as_ref().is_some_and(|s| s.identity.background),
            connected_seconds: session.as_ref().map(|s| s.uptime().as_secs()),
            pending_requests: pending,
            last_error: self.last_error(),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<Session>> {
        self.current.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<Session>> {
        self.current.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Build a [`Session`] from a completed handshake.
pub fn session_from_ack(ack: HelloAck, outbound: mpsc::Sender<Envelope>, peer: String) -> Session {
    Session {
        id: ack.session_id,
        identity: Arc::new(ack.identity),
        capabilities: Arc::new(ack.capabilities),
        outbound,
        connected_at: Instant::now(),
        peer,
    }
}

#[cfg(test)]
mod tests {
    use blender_protocol::version::BlenderVersion;

    use super::*;

    fn session(id: SessionId) -> Session {
        let (tx, _rx) = mpsc::channel(1);
        Session {
            id,
            identity: Arc::new(BlenderIdentity {
                blender_version: BlenderVersion::new(5, 1, 0),
                python_version: "3.13.0".into(),
                addon_version: "0.1.0".into(),
                platform: "windows".into(),
                background: false,
            }),
            capabilities: Arc::new(Capabilities::default()),
            outbound: tx,
            connected_at: Instant::now(),
            peer: "127.0.0.1:1234".into(),
        }
    }

    #[test]
    fn requires_a_connection() {
        let slot = SessionSlot::new();
        assert_eq!(
            slot.require().unwrap_err().code,
            ErrorCode::BlenderNotConnected
        );
        slot.replace(session(SessionId::new()));
        assert!(slot.require().is_ok());
    }

    #[test]
    fn a_stale_teardown_does_not_clear_the_new_session() {
        let slot = SessionSlot::new();
        let old = SessionId::new();
        let new = SessionId::new();
        slot.replace(session(old));
        slot.replace(session(new));

        assert!(
            !slot.clear_if(old, "old socket closed"),
            "old id must not match"
        );
        assert!(slot.get().is_some(), "the new session must survive");

        assert!(slot.clear_if(new, "Blender exited"));
        assert!(slot.get().is_none());
        assert_eq!(slot.last_error().as_deref(), Some("Blender exited"));
    }

    #[test]
    fn status_reflects_the_connection() {
        let slot = SessionSlot::new();
        let disconnected = slot.status("127.0.0.1:9877", 0);
        assert!(!disconnected.connected);
        assert!(disconnected.blender_version.is_none());

        slot.replace(session(SessionId::new()));
        let connected = slot.status("127.0.0.1:9877", 3);
        assert!(connected.connected);
        assert_eq!(connected.blender_version.as_deref(), Some("5.1.0"));
        assert_eq!(connected.pending_requests, 3);
    }
}

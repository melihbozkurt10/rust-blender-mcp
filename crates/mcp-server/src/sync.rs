//! Keeping the scene cache in step with Blender.
//!
//! The user is editing the same file. This task listens to the bridge's event
//! stream and folds every change into the cache, so `scene.diff` can answer
//! "what happened since I last looked" without re-reading the scene.

use std::sync::Arc;

use blender_protocol::envelope::SessionId;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

/// Start the event pump. Runs until the server stops.
pub fn spawn(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    // Subscribe here rather than inside the task: a broadcast channel only
    // delivers to receivers that already exist, so subscribing after the spawn
    // returns would silently drop anything that happened in between.
    let mut events = state.client.subscribe();

    tokio::spawn(async move {
        let mut session: Option<SessionId> = None;

        loop {
            match events.recv().await {
                Ok(event) => {
                    // A new session means a different Blender, or the same one
                    // restarted. Either way the previous record is not
                    // comparable with what follows.
                    if session != Some(event.session_id) {
                        // The session starts just *before* this event, so the
                        // event itself falls inside any diff taken from the
                        // session's opening revision.
                        state.cache.begin_session(
                            event.session_id.to_string(),
                            scene_cache::Revision(event.revision.saturating_sub(1)),
                        );
                        session = Some(event.session_id);
                        tracing::info!(session = %event.session_id, "tracking a new Blender session");
                    }
                    state.cache.apply(&event);
                }
                Err(RecvError::Lagged(missed)) => {
                    // The cache has holes now, and pretending otherwise would
                    // make every later diff quietly wrong. Resetting forces the
                    // next diff to report REVISION_EXPIRED, which tells the
                    // caller to re-read rather than trust a partial answer.
                    tracing::warn!(missed, "fell behind the event stream; resetting the cache");
                    state.cache.end_session();
                    session = None;
                }
                Err(RecvError::Closed) => {
                    tracing::debug!("event stream closed");
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use blender_client::BlenderClient;
    use blender_protocol::{
        command::Category,
        event::{Event, EventPayload},
        ids::AnyKind,
    };
    use tokio::net::TcpListener;

    use super::*;
    use crate::{
        config::Config,
        registry::{Activation, Registry},
    };

    async fn state() -> Arc<AppState> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client = BlenderClient::from_listener(blender_client::Config::default(), listener);
        let registry = Arc::new(Registry::new(vec![], Activation::lazy(&[Category::Core])));
        AppState::new(Config::default(), client, registry)
    }

    #[tokio::test]
    async fn the_cache_starts_empty_and_answerable() {
        let state = state().await;
        let snapshot = state.cache.snapshot();
        assert_eq!(snapshot.revision, 0);
        assert!(
            state
                .cache
                .diff(scene_cache::Revision(0), None)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn events_reach_the_cache_through_the_pump() {
        let state = state().await;
        let handle = spawn(Arc::clone(&state));

        // The pump reads from the same broadcast channel the transport writes
        // to, so publishing directly exercises the real path.
        let session = SessionId::new();
        let sender = state.client.event_sender();
        sender
            .send(Event {
                session_id: session,
                revision: 1,
                payload: EventPayload::Created {
                    kind: AnyKind::ObjectKind,
                    id: "abc".into(),
                    name: "Cube".into(),
                },
            })
            .expect("a subscriber exists");

        // Give the pump a moment; this is the one place a sleep is honest,
        // because the handoff is between tasks with no completion signal.
        for _ in 0..50 {
            if state.cache.revision().value() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(state.cache.revision(), scene_cache::Revision(1));
        let diff = state.cache.diff(scene_cache::Revision(0), None).unwrap();
        assert_eq!(diff.created.len(), 1);
        assert_eq!(diff.created[0].id, "abc");

        handle.abort();
    }
}

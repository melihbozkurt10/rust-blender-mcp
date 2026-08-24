//! In-flight request bookkeeping.
//!
//! Several requests can be outstanding at once and Blender may answer them in
//! any order, so responses are matched by `request_id` rather than by
//! position. When a connection dies, every waiter is failed explicitly instead
//! of being left to time out.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use blender_protocol::{
    envelope::{RequestId, Response, SessionId},
    error::{BlenderError, ErrorCode},
};
use serde_json::Value;
use tokio::sync::oneshot;

type Waiter = oneshot::Sender<Result<Response, BlenderError>>;

/// Registry of requests awaiting a response.
#[derive(Clone, Default)]
pub struct PendingRequests {
    inner: Arc<Mutex<HashMap<RequestId, Entry>>>,
}

struct Entry {
    waiter: Waiter,
    /// Session the request was sent on. A response arriving on a *different*
    /// session belongs to a Blender instance that has since been replaced.
    session: SessionId,
    op: String,
}

impl PendingRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request and get the receiver its response will arrive on.
    pub fn register(
        &self,
        id: RequestId,
        session: SessionId,
        op: impl Into<String>,
    ) -> oneshot::Receiver<Result<Response, BlenderError>> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.lock();
        guard.insert(
            id,
            Entry {
                waiter: tx,
                session,
                op: op.into(),
            },
        );
        rx
    }

    /// Deliver a response. Returns `false` when nothing was waiting for it,
    /// which happens for a timed-out request or a duplicate answer.
    pub fn complete(&self, session: SessionId, response: Response) -> bool {
        let entry = {
            let mut guard = self.lock();
            guard.remove(&response.request_id)
        };
        let Some(entry) = entry else { return false };
        if entry.session != session {
            // A late answer from a replaced Blender instance. Failing the
            // waiter is wrong (its request went to the current session and may
            // still be answered), so put it back and drop the stray response.
            let mut guard = self.lock();
            guard.insert(response.request_id, entry);
            tracing::warn!(
                request_id = %response.request_id,
                "discarding a response from a superseded session"
            );
            return false;
        }
        // A receiver dropped by a timed-out caller makes `send` fail; that is
        // expected and not worth reporting.
        entry.waiter.send(Ok(response)).is_ok()
    }

    /// Cancel one request, e.g. after a timeout.
    pub fn cancel(&self, id: RequestId) -> bool {
        self.lock().remove(&id).is_some()
    }

    /// Fail every outstanding request, because the connection is gone.
    pub fn fail_all(&self, reason: &BlenderError) {
        let entries: Vec<(RequestId, Entry)> = {
            let mut guard = self.lock();
            guard.drain().collect()
        };
        if entries.is_empty() {
            return;
        }
        tracing::warn!(
            count = entries.len(),
            "failing in-flight requests: {}",
            reason.message
        );
        for (id, entry) in entries {
            let error = reason
                .clone()
                .with_detail("request_id", id.to_string())
                .with_detail("op", entry.op);
            let _ = entry.waiter.send(Err(error));
        }
    }

    /// Fail only the requests belonging to a session that has ended.
    pub fn fail_session(&self, session: SessionId, reason: &BlenderError) {
        let entries: Vec<(RequestId, Entry)> = {
            let mut guard = self.lock();
            let ids: Vec<RequestId> = guard
                .iter()
                .filter(|(_, e)| e.session == session)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.remove(&id).map(|e| (id, e)))
                .collect()
        };
        for (id, entry) in entries {
            let error = reason
                .clone()
                .with_detail("request_id", id.to_string())
                .with_detail("op", entry.op);
            let _ = entry.waiter.send(Err(error));
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RequestId, Entry>> {
        // The map is only ever held for a map operation, never across an
        // await, so poisoning can only follow a panic inside one of those --
        // recovering is strictly better than cascading the panic into every
        // other request.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The error every in-flight request is failed with when Blender disconnects.
pub fn connection_lost(detail: impl Into<String>) -> BlenderError {
    BlenderError::new(ErrorCode::ConnectionLost, detail.into())
}

/// The error a request is failed with when it outlives its deadline.
pub fn timed_out(op: &str, millis: u64) -> BlenderError {
    BlenderError::new(
        ErrorCode::Timeout,
        format!("`{op}` did not complete within {millis} ms."),
    )
    .with_detail("op", op)
    .with_detail("timeout_ms", millis)
}

/// Helper for extracting a result value from a response.
pub fn into_value(response: Response) -> Result<Value, BlenderError> {
    response.into_result()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(id: RequestId) -> Response {
        Response::success(id, serde_json::json!({"ok": true}))
    }

    #[tokio::test]
    async fn matches_responses_out_of_order() {
        let pending = PendingRequests::new();
        let session = SessionId::new();
        let first = RequestId::new();
        let second = RequestId::new();
        let rx1 = pending.register(first, session, "object.list");
        let rx2 = pending.register(second, session, "scene.summary");

        assert!(pending.complete(session, response(second)));
        assert!(pending.complete(session, response(first)));

        assert_eq!(rx1.await.unwrap().unwrap().request_id, first);
        assert_eq!(rx2.await.unwrap().unwrap().request_id, second);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn disconnect_fails_every_waiter() {
        let pending = PendingRequests::new();
        let session = SessionId::new();
        let rx = pending.register(RequestId::new(), session, "render.execute");
        pending.fail_all(&connection_lost("Blender exited"));
        let err = rx.await.unwrap().unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionLost);
        assert_eq!(err.details["op"], "render.execute");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn responses_from_a_superseded_session_are_ignored() {
        let pending = PendingRequests::new();
        let current = SessionId::new();
        let stale = SessionId::new();
        let id = RequestId::new();
        let rx = pending.register(id, current, "object.get");

        assert!(
            !pending.complete(stale, response(id)),
            "stale session must not resolve"
        );
        assert_eq!(pending.len(), 1, "the request must stay registered");

        assert!(pending.complete(current, response(id)));
        assert!(rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn unknown_response_ids_are_reported_not_panicked() {
        let pending = PendingRequests::new();
        assert!(!pending.complete(SessionId::new(), response(RequestId::new())));
    }

    #[tokio::test]
    async fn only_the_dead_session_is_failed() {
        let pending = PendingRequests::new();
        let alive = SessionId::new();
        let dead = SessionId::new();
        let alive_rx = pending.register(RequestId::new(), alive, "a");
        let dead_rx = pending.register(RequestId::new(), dead, "b");

        pending.fail_session(dead, &connection_lost("gone"));

        assert!(dead_rx.await.unwrap().is_err());
        assert_eq!(pending.len(), 1);
        drop(alive_rx);
    }
}

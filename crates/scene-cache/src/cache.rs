//! The cache itself.

use std::sync::RwLock;

use blender_protocol::{
    Result,
    event::{Event, EventPayload},
};

use crate::{
    diff::{Change, SceneDiff, fold},
    invalidation::Invalidation,
    revision::{Revision, RevisionHistory},
    snapshot::Snapshot,
};

/// Tracks what has changed in the scene, and answers questions about it.
///
/// Thread-safe and cheap to share. The lock is only ever held for the length of
/// one map operation, never across an await.
pub struct SceneCache {
    inner: RwLock<Inner>,
    capacity: usize,
}

struct Inner {
    history: RevisionHistory<Change>,
    /// The session whose events are being applied. Events from a previous
    /// Blender connection are dropped rather than mixed in.
    session: Option<String>,
    scene: Option<String>,
}

impl SceneCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                history: RevisionHistory::new(capacity),
                session: None,
                scene: None,
            }),
            capacity: capacity.max(1),
        }
    }

    /// The current revision.
    pub fn revision(&self) -> Revision {
        self.read().history.current()
    }

    /// A marker to diff against later.
    pub fn snapshot(&self) -> Snapshot {
        let inner = self.read();
        let snapshot = Snapshot::new(
            inner.history.current(),
            inner.history.horizon(),
            self.capacity,
        );
        match &inner.scene {
            Some(scene) => snapshot.with_scene(scene.clone()),
            None => snapshot,
        }
    }

    /// Start tracking a new Blender session.
    ///
    /// Anything recorded for a previous one is discarded: ids are still valid,
    /// but the revision numbering restarts with the bridge and mixing the two
    /// would produce nonsense diffs.
    pub fn begin_session(&self, session: impl Into<String>, revision: Revision) {
        let mut inner = self.write();
        inner.session = Some(session.into());
        inner.history.observe(revision);
        inner.history.reset();
    }

    /// Forget the session, keeping the counter.
    pub fn end_session(&self) {
        let mut inner = self.write();
        inner.session = None;
        inner.history.reset();
    }

    /// Apply one event from the bridge.
    ///
    /// Returns what it invalidated, so a caller can drop the right cached
    /// state, or `None` if the event was ignored.
    pub fn apply(&self, event: &Event) -> Option<Invalidation> {
        let mut inner = self.write();

        // An event from a session that has been replaced describes a Blender
        // that is no longer there.
        if let Some(current) = &inner.session
            && current != &event.session_id.to_string()
        {
            tracing::debug!("dropping an event from a superseded session");
            return None;
        }

        let invalidation = Invalidation::of(&event.payload);
        let change = to_change(&event.payload);

        if let Some(change) = change {
            let reset = change.is_reset();
            inner.history.push_at(Revision(event.revision), change);
            if reset {
                // Keep the marker in history so a diff spanning it reports the
                // reset, but drop everything before it.
                inner.scene = scene_name(&event.payload).or_else(|| inner.scene.clone());
            }
        } else {
            // An event the cache does not model still moves the scene on.
            inner.history.observe(Revision(event.revision));
        }

        Some(invalidation)
    }

    /// What changed between two revisions.
    pub fn diff(&self, from: Revision, to: Option<Revision>) -> Result<SceneDiff> {
        let inner = self.read();
        let to = to.unwrap_or_else(|| inner.history.current());
        let entries = inner.history.since(from, to)?;
        let changes: Vec<Change> = entries
            .into_iter()
            .map(|entry| entry.change.clone())
            .collect();
        Ok(fold(from, to, changes))
    }

    /// How many changes are being remembered.
    pub fn len(&self) -> usize {
        self.read().history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The oldest revision a diff can start from.
    pub fn oldest_revision(&self) -> Revision {
        self.read().history.horizon()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for SceneCache {
    fn default() -> Self {
        Self::new(1000)
    }
}

fn scene_name(payload: &EventPayload) -> Option<String> {
    match payload {
        EventPayload::SceneChanged { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Turn a bridge event into a recorded change.
fn to_change(payload: &EventPayload) -> Option<Change> {
    Some(match payload {
        EventPayload::Created { kind, id, name } => Change::Created {
            kind: *kind,
            id: id.clone(),
            name: name.clone(),
        },
        EventPayload::Deleted { kind, id, name } => Change::Deleted {
            kind: *kind,
            id: id.clone(),
            name: name.clone(),
        },
        EventPayload::Renamed { kind, id, from, to } => Change::Renamed {
            kind: *kind,
            id: id.clone(),
            from: from.clone(),
            to: to.clone(),
        },
        EventPayload::Modified { kind, id, fields } => Change::Modified {
            kind: *kind,
            id: id.clone(),
            fields: fields.clone(),
        },
        EventPayload::MeshInvalidated {
            object_id,
            mesh_revision,
        } => Change::MeshInvalidated {
            id: object_id.clone(),
            mesh_revision: *mesh_revision,
        },
        EventPayload::NodeTreeInvalidated { node_tree_id } => Change::NodeTreeInvalidated {
            id: node_tree_id.clone(),
        },
        EventPayload::SelectionChanged { selected, active } => Change::SelectionChanged {
            selected: selected.clone(),
            active: active.clone(),
        },
        EventPayload::FileReloaded { filepath } => Change::Reset {
            reason: filepath
                .as_ref()
                .map(|path| format!("a file was loaded: {path}"))
                .unwrap_or_else(|| "the file was reloaded".to_string()),
        },
        EventPayload::SceneChanged { name, .. } => Change::Reset {
            reason: format!("the active scene changed to `{name}`"),
        },
    })
}

#[cfg(test)]
mod tests {
    use blender_protocol::{
        ErrorCode,
        envelope::SessionId,
        event::{Event, ModifiedField},
        ids::AnyKind,
    };

    use super::*;

    fn event(session: SessionId, revision: u64, payload: EventPayload) -> Event {
        Event {
            session_id: session,
            revision,
            payload,
        }
    }

    fn created(id: &str) -> EventPayload {
        EventPayload::Created {
            kind: AnyKind::ObjectKind,
            id: id.into(),
            name: format!("Object{id}"),
        }
    }

    #[test]
    fn a_fresh_cache_has_nothing_to_report() {
        let cache = SceneCache::new(10);
        assert_eq!(cache.revision(), Revision::ZERO);
        assert!(cache.diff(Revision::ZERO, None).unwrap().is_empty());
    }

    #[test]
    fn events_become_diffable_changes() {
        let cache = SceneCache::new(10);
        let session = SessionId::new();
        cache.begin_session(session.to_string(), Revision::ZERO);

        let start = cache.snapshot();
        cache.apply(&event(session, 1, created("a")));
        cache.apply(&event(
            session,
            2,
            EventPayload::Modified {
                kind: AnyKind::ObjectKind,
                id: "b".into(),
                fields: vec![ModifiedField::Transform],
            },
        ));

        let diff = cache.diff(Revision(start.revision), None).unwrap();
        assert_eq!(diff.created.len(), 1);
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.to_revision, 2);
    }

    #[test]
    fn events_from_a_superseded_session_are_ignored() {
        let cache = SceneCache::new(10);
        let current = SessionId::new();
        let stale = SessionId::new();
        cache.begin_session(current.to_string(), Revision::ZERO);

        assert!(cache.apply(&event(stale, 1, created("a"))).is_none());
        assert!(cache.diff(Revision::ZERO, None).unwrap().is_empty());

        assert!(cache.apply(&event(current, 1, created("a"))).is_some());
        assert_eq!(cache.diff(Revision::ZERO, None).unwrap().created.len(), 1);
    }

    #[test]
    fn a_file_reload_is_reported_as_a_reset() {
        let cache = SceneCache::new(10);
        let session = SessionId::new();
        cache.begin_session(session.to_string(), Revision::ZERO);

        cache.apply(&event(session, 1, created("a")));
        let invalidation = cache
            .apply(&event(
                session,
                2,
                EventPayload::FileReloaded { filepath: None },
            ))
            .unwrap();
        assert!(invalidation.is_total());

        let diff = cache.diff(Revision::ZERO, None).unwrap();
        assert!(diff.reset);
        assert!(
            diff.created.is_empty(),
            "what came before the reload is gone"
        );
    }

    #[test]
    fn a_diff_from_too_far_back_expires_rather_than_lying() {
        let cache = SceneCache::new(3);
        let session = SessionId::new();
        cache.begin_session(session.to_string(), Revision::ZERO);
        for index in 1..=6 {
            cache.apply(&event(session, index, created(&index.to_string())));
        }

        let error = cache.diff(Revision(1), None).unwrap_err();
        assert_eq!(error.code, ErrorCode::RevisionExpired);
        assert!(cache.diff(cache.oldest_revision(), None).is_ok());
    }

    #[test]
    fn a_new_session_restarts_the_record_without_rewinding_the_counter() {
        let cache = SceneCache::new(10);
        let first = SessionId::new();
        cache.begin_session(first.to_string(), Revision::ZERO);
        cache.apply(&event(first, 1, created("a")));
        cache.apply(&event(first, 2, created("b")));
        let before = cache.revision();

        let second = SessionId::new();
        cache.begin_session(second.to_string(), Revision::ZERO);

        assert_eq!(
            cache.revision(),
            before,
            "the counter must not go backwards"
        );
        assert!(
            cache.is_empty(),
            "the previous session's record is not comparable"
        );
    }

    #[test]
    fn the_cache_follows_the_bridge_revision_when_it_runs_ahead() {
        let cache = SceneCache::new(10);
        let session = SessionId::new();
        cache.begin_session(session.to_string(), Revision::ZERO);

        // The bridge reports revision 100 for its first event, because the user
        // did ninety-nine things before the server connected.
        cache.apply(&event(session, 100, created("a")));
        assert_eq!(cache.revision(), Revision(100));
    }

    #[test]
    fn a_snapshot_reports_its_own_headroom() {
        let cache = SceneCache::new(5);
        let session = SessionId::new();
        cache.begin_session(session.to_string(), Revision::ZERO);
        let snapshot = cache.snapshot();
        assert_eq!(snapshot.history_capacity, 5);

        for index in 1..=3 {
            cache.apply(&event(session, index, created(&index.to_string())));
        }
        assert_eq!(snapshot.headroom(cache.revision()), 2);
    }
}

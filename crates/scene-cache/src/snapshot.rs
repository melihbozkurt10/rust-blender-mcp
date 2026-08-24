//! A point-in-time marker a caller can diff against later.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::revision::Revision;

/// What `scene.snapshot` hands back.
///
/// Deliberately just a revision plus enough context to explain a later
/// `REVISION_EXPIRED`: the scene itself is not copied, because copying a scene
/// to answer "what changed" would cost more than re-reading it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    pub revision: u64,
    /// The oldest revision that can still be diffed from, at the time this
    /// snapshot was taken.
    pub oldest_revision: u64,
    /// How many revisions of history the server keeps.
    pub history_capacity: usize,
    /// Which scene it was taken in. A snapshot from another scene cannot be
    /// diffed meaningfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
}

impl Snapshot {
    pub fn new(revision: Revision, oldest: Revision, capacity: usize) -> Self {
        Self {
            revision: revision.value(),
            oldest_revision: oldest.value(),
            history_capacity: capacity,
            scene: None,
        }
    }

    pub fn with_scene(mut self, scene: impl Into<String>) -> Self {
        self.scene = Some(scene.into());
        self
    }

    /// How many revisions of headroom remain before this snapshot expires.
    ///
    /// Lets a caller decide to diff sooner rather than discovering too late
    /// that its marker has aged out.
    pub fn headroom(&self, current: Revision) -> u64 {
        let elapsed = current.value().saturating_sub(self.revision);
        (self.history_capacity as u64).saturating_sub(elapsed)
    }

    /// Whether a diff from this snapshot would still be answerable.
    pub fn is_answerable(&self, oldest: Revision) -> bool {
        self.revision >= oldest.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_records_where_the_window_was() {
        let snapshot = Snapshot::new(Revision(42), Revision(10), 1000);
        assert_eq!(snapshot.revision, 42);
        assert_eq!(snapshot.oldest_revision, 10);
    }

    #[test]
    fn headroom_shrinks_as_the_scene_moves_on() {
        let snapshot = Snapshot::new(Revision(100), Revision(0), 50);
        assert_eq!(snapshot.headroom(Revision(100)), 50);
        assert_eq!(snapshot.headroom(Revision(120)), 30);
        assert_eq!(
            snapshot.headroom(Revision(200)),
            0,
            "saturates rather than wrapping"
        );
    }

    #[test]
    fn answerability_follows_the_history_window() {
        let snapshot = Snapshot::new(Revision(10), Revision(0), 100);
        assert!(snapshot.is_answerable(Revision(5)));
        assert!(snapshot.is_answerable(Revision(10)));
        assert!(!snapshot.is_answerable(Revision(11)));
    }
}

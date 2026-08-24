//! The revision counter and its bounded history.

use std::collections::VecDeque;

use blender_protocol::{BlenderError, ErrorCode, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A point in the scene's history.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const ZERO: Revision = Revision(0);

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Revision {
        Revision(self.0 + 1)
    }
}

impl std::fmt::Display for Revision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Revision {
    fn from(value: u64) -> Self {
        Revision(value)
    }
}

/// One recorded change, tagged with the revision it happened at.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Entry<T> {
    pub revision: Revision,
    pub change: T,
}

/// A bounded window of history.
///
/// Old entries fall off the back. That is a feature: unbounded history in a
/// long-lived server is a slow memory leak, and a caller asking about something
/// that far back should be told to resynchronise rather than given a partial
/// answer that looks complete.
#[derive(Debug, Clone)]
pub struct RevisionHistory<T> {
    entries: VecDeque<Entry<T>>,
    capacity: usize,
    current: Revision,
    /// The oldest revision still answerable.
    horizon: Revision,
}

impl<T> RevisionHistory<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
            current: Revision::ZERO,
            horizon: Revision::ZERO,
        }
    }

    pub fn current(&self) -> Revision {
        self.current
    }

    /// The oldest revision a diff can start from.
    pub fn horizon(&self) -> Revision {
        self.horizon
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a change at the next revision.
    pub fn push(&mut self, change: T) -> Revision {
        self.current = self.current.next();
        self.entries.push_back(Entry {
            revision: self.current,
            change,
        });
        while self.entries.len() > self.capacity {
            if let Some(dropped) = self.entries.pop_front() {
                // Anything at or before the dropped revision can no longer be
                // answered completely.
                self.horizon = dropped.revision;
            }
        }
        self.current
    }

    /// Record a change that the peer has already numbered.
    ///
    /// The bridge is authoritative about revision numbers -- it counts changes
    /// the user makes by hand too -- so when an event arrives carrying one, the
    /// cache adopts it rather than inventing a number of its own. Inventing one
    /// would double-count every change that came in over the wire.
    pub fn push_at(&mut self, revision: Revision, change: T) -> Revision {
        self.entries.push_back(Entry { revision, change });
        if revision > self.current {
            self.current = revision;
        }
        while self.entries.len() > self.capacity {
            if let Some(dropped) = self.entries.pop_front() {
                self.horizon = dropped.revision;
            }
        }
        self.current
    }

    /// Move the counter forward without recording anything.
    ///
    /// Used when the bridge reports a revision higher than the cache knows
    /// about, which happens when a change arrived that the cache does not
    /// model.
    pub fn observe(&mut self, revision: Revision) {
        if revision > self.current {
            self.current = revision;
            // A jump means changes happened that were not recorded, so nothing
            // before this point can be diffed reliably.
            self.horizon = revision;
            self.entries.clear();
        }
    }

    /// Changes strictly after `from`, up to and including `to`.
    pub fn since(&self, from: Revision, to: Revision) -> Result<Vec<&Entry<T>>> {
        if from > to {
            return Err(BlenderError::invalid_argument(format!(
                "`from_revision` {from} is after `to_revision` {to}."
            )));
        }
        if to > self.current {
            return Err(BlenderError::invalid_argument(format!(
                "Revision {to} is in the future; the scene is at {}.",
                self.current
            ))
            .with_detail("current_revision", self.current.value()));
        }
        if from < self.horizon {
            return Err(BlenderError::new(
                ErrorCode::RevisionExpired,
                format!(
                    "Revision {from} has fallen out of the {} entry history; the oldest \
                     answerable revision is {}. Take a fresh snapshot and diff from there.",
                    self.capacity, self.horizon
                ),
            )
            .with_detail("requested_revision", from.value())
            .with_detail("oldest_revision", self.horizon.value())
            .with_detail("current_revision", self.current.value()));
        }

        Ok(self
            .entries
            .iter()
            .filter(|entry| entry.revision > from && entry.revision <= to)
            .collect())
    }

    /// Forget everything, keeping the counter.
    ///
    /// Used when a file is loaded or the scene changes: the counter must not go
    /// backwards, but nothing recorded before is meaningful any more.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.horizon = self.current;
    }
}

impl<T> Default for RevisionHistory<T> {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_advance_one_at_a_time() {
        let mut history: RevisionHistory<&str> = RevisionHistory::new(10);
        assert_eq!(history.current(), Revision::ZERO);
        assert_eq!(history.push("a"), Revision(1));
        assert_eq!(history.push("b"), Revision(2));
        assert_eq!(history.current(), Revision(2));
    }

    #[test]
    fn push_at_adopts_the_peer_numbering_without_double_counting() {
        let mut history: RevisionHistory<&str> = RevisionHistory::new(10);
        history.push_at(Revision(5), "a");
        assert_eq!(
            history.current(),
            Revision(5),
            "the peer number is taken as-is"
        );
        history.push_at(Revision(6), "b");
        assert_eq!(history.current(), Revision(6));

        let changes = history.since(Revision(4), Revision(6)).unwrap();
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn push_at_never_rewinds_the_counter() {
        let mut history: RevisionHistory<&str> = RevisionHistory::new(10);
        history.push_at(Revision(10), "a");
        history.push_at(Revision(3), "late");
        assert_eq!(history.current(), Revision(10));
    }

    #[test]
    fn since_returns_the_half_open_range() {
        let mut history: RevisionHistory<&str> = RevisionHistory::new(10);
        history.push("a");
        history.push("b");
        history.push("c");

        let changes = history.since(Revision(1), Revision(3)).unwrap();
        assert_eq!(changes.len(), 2, "from is exclusive, to is inclusive");
        assert_eq!(changes[0].change, "b");
        assert_eq!(changes[1].change, "c");
    }

    #[test]
    fn asking_about_the_current_revision_returns_nothing() {
        let mut history: RevisionHistory<&str> = RevisionHistory::new(10);
        history.push("a");
        assert!(history.since(Revision(1), Revision(1)).unwrap().is_empty());
    }

    #[test]
    fn history_is_bounded_and_expiry_is_explicit() {
        let mut history: RevisionHistory<u32> = RevisionHistory::new(3);
        for value in 0..6 {
            history.push(value);
        }
        assert_eq!(history.len(), 3);

        let error = history.since(Revision(1), Revision(6)).unwrap_err();
        assert_eq!(error.code, ErrorCode::RevisionExpired);
        assert_eq!(error.details["oldest_revision"], history.horizon().value());

        // Anything inside the window still works.
        assert!(history.since(history.horizon(), Revision(6)).is_ok());
    }

    #[test]
    fn a_future_revision_is_rejected_as_an_argument_error() {
        let mut history: RevisionHistory<u32> = RevisionHistory::new(10);
        history.push(1);
        let error = history.since(Revision(1), Revision(99)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["current_revision"], 1);
    }

    #[test]
    fn an_inverted_range_is_rejected() {
        let history: RevisionHistory<u32> = RevisionHistory::new(10);
        assert!(history.since(Revision(5), Revision(1)).is_err());
    }

    #[test]
    fn observing_a_jump_invalidates_earlier_history() {
        let mut history: RevisionHistory<u32> = RevisionHistory::new(10);
        history.push(1);
        history.push(2);
        history.observe(Revision(50));

        assert_eq!(history.current(), Revision(50));
        assert!(
            history.is_empty(),
            "unrecorded changes cannot be diffed over"
        );
        assert_eq!(
            history.since(Revision(2), Revision(50)).unwrap_err().code,
            ErrorCode::RevisionExpired
        );
    }

    #[test]
    fn observing_an_older_revision_changes_nothing() {
        let mut history: RevisionHistory<u32> = RevisionHistory::new(10);
        history.push(1);
        history.push(2);
        history.observe(Revision(1));
        assert_eq!(history.current(), Revision(2));
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn reset_keeps_the_counter_but_drops_the_record() {
        let mut history: RevisionHistory<u32> = RevisionHistory::new(10);
        history.push(1);
        history.push(2);
        history.reset();
        assert_eq!(
            history.current(),
            Revision(2),
            "the counter must never go backwards"
        );
        assert!(history.is_empty());
    }
}

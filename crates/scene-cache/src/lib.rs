//! Scene revision tracking, snapshots and diffs.
//!
//! The person at the keyboard is editing the same file as the model. Without
//! something watching, a cached idea of the scene drifts from reality within
//! seconds. This crate keeps a small, bounded record of what changed and when,
//! so a caller can ask "what has happened since I last looked?" and get an
//! answer instead of re-reading the whole scene.
//!
//! Two deliberate limits, both visible in the API rather than buried:
//!
//! * History is bounded. Asking about a revision that has fallen off the back
//!   returns `REVISION_EXPIRED`, not a wrong answer.
//! * Meshes and node graphs are *invalidated*, not diffed. Serialising every
//!   vertex on every depsgraph tick would cost far more than it saves, so those
//!   are reported as "this is stale, re-read it".

#![forbid(unsafe_code)]

pub mod cache;
pub mod diff;
pub mod invalidation;
pub mod revision;
pub mod snapshot;

pub use cache::SceneCache;
pub use diff::{Change, SceneDiff};
pub use invalidation::Invalidation;
pub use revision::{Revision, RevisionHistory};
pub use snapshot::Snapshot;

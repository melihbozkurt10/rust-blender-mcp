//! Unsolicited notifications from Blender.
//!
//! The user is editing the same scene by hand. Without events the server's
//! cache would drift from reality within seconds, so the add-on watches the
//! depsgraph and reports coarse changes.
//!
//! Coarse is deliberate: serialising every node tweak on every depsgraph
//! update would cost more than it saves. Fine-grained data (meshes, node
//! graphs) is *invalidated* instead, and re-read on demand.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{envelope::SessionId, ids::AnyKind};

/// One change notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Session that produced this event. Events from a previous session are
    /// dropped rather than applied to the current cache.
    pub session_id: SessionId,
    /// Scene revision after the change.
    pub revision: u64,
    #[serde(flatten)]
    pub payload: EventPayload,
}

/// What changed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventPayload {
    /// An entity appeared.
    Created {
        kind: AnyKind,
        id: String,
        name: String,
    },
    /// An entity was removed.
    Deleted {
        kind: AnyKind,
        id: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// An entity was renamed. Its id is unchanged.
    Renamed {
        kind: AnyKind,
        id: String,
        from: String,
        to: String,
    },
    /// One or more cached fields of an entity changed. `fields` names them so
    /// the cache can invalidate precisely.
    Modified {
        kind: AnyKind,
        id: String,
        #[serde(default)]
        fields: Vec<ModifiedField>,
    },
    /// The selection or active object changed.
    SelectionChanged {
        #[serde(default)]
        selected: Vec<String>,
        #[serde(default)]
        active: Option<String>,
    },
    /// The active scene changed; the whole cache is suspect.
    SceneChanged { scene_id: String, name: String },
    /// A mesh's topology changed. Cached vertex/face indices are now stale.
    MeshInvalidated {
        object_id: String,
        mesh_revision: u64,
    },
    /// A node tree changed in a way too fine-grained to describe. Re-read it.
    NodeTreeInvalidated { node_tree_id: String },
    /// A new .blend was loaded, or the file was reverted. Drop everything.
    FileReloaded {
        #[serde(default)]
        filepath: Option<String>,
    },
}

/// Which cached aspect of an entity changed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModifiedField {
    Transform,
    Name,
    Visibility,
    Parent,
    Materials,
    Modifiers,
    Constraints,
    MeshSummary,
    Animation,
    Collections,
    Data,
}

impl EventPayload {
    /// The entity this event is about, when it names exactly one.
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            EventPayload::Created { id, .. }
            | EventPayload::Deleted { id, .. }
            | EventPayload::Renamed { id, .. }
            | EventPayload::Modified { id, .. } => Some(id),
            EventPayload::MeshInvalidated { object_id, .. } => Some(object_id),
            EventPayload::NodeTreeInvalidated { node_tree_id } => Some(node_tree_id),
            EventPayload::SceneChanged { scene_id, .. } => Some(scene_id),
            EventPayload::SelectionChanged { .. } | EventPayload::FileReloaded { .. } => None,
        }
    }

    /// Whether this event invalidates cached state wholesale.
    pub const fn is_global_invalidation(&self) -> bool {
        matches!(
            self,
            EventPayload::FileReloaded { .. } | EventPayload::SceneChanged { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_internally_tagged() {
        let ev = Event {
            session_id: SessionId::new(),
            revision: 7,
            payload: EventPayload::Renamed {
                kind: AnyKind::ObjectKind,
                id: "1e6f".into(),
                from: "Cube".into(),
                to: "Wall".into(),
            },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["event"], "renamed");
        assert_eq!(json["revision"], 7);
        assert_eq!(json["kind"], "object");
    }

    #[test]
    fn file_reload_invalidates_everything() {
        assert!(EventPayload::FileReloaded { filepath: None }.is_global_invalidation());
        assert!(
            !EventPayload::MeshInvalidated {
                object_id: "x".into(),
                mesh_revision: 1
            }
            .is_global_invalidation()
        );
    }
}

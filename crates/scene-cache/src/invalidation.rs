//! What a change makes stale.
//!
//! The cache holds cheap summaries. When something changes, the question is not
//! "what is the new value" but "what do I no longer trust". Answering that
//! precisely is what lets the server avoid re-reading a whole scene because one
//! object moved.

use blender_protocol::{
    event::{EventPayload, ModifiedField},
    ids::AnyKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a change invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Invalidation {
    /// Nothing cached is affected.
    None,
    /// One entity's summary is stale.
    Entity { kind: AnyKind, id: String },
    /// One mesh's element indices are stale.
    MeshTopology { id: String },
    /// One node graph is stale.
    NodeTree { id: String },
    /// The selection is stale.
    Selection,
    /// Everything is stale.
    Everything { reason: String },
}

impl Invalidation {
    /// What an event from the bridge invalidates.
    pub fn of(event: &EventPayload) -> Self {
        match event {
            EventPayload::FileReloaded { filepath } => Invalidation::Everything {
                reason: filepath
                    .as_ref()
                    .map(|path| format!("a file was loaded: {path}"))
                    .unwrap_or_else(|| "the file was reloaded".to_string()),
            },
            EventPayload::SceneChanged { name, .. } => Invalidation::Everything {
                reason: format!("the active scene changed to `{name}`"),
            },
            EventPayload::Created { kind, id, .. }
            | EventPayload::Deleted { kind, id, .. }
            | EventPayload::Renamed { kind, id, .. }
            | EventPayload::Modified { kind, id, .. } => Invalidation::Entity {
                kind: *kind,
                id: id.clone(),
            },
            EventPayload::MeshInvalidated { object_id, .. } => Invalidation::MeshTopology {
                id: object_id.clone(),
            },
            EventPayload::NodeTreeInvalidated { node_tree_id } => Invalidation::NodeTree {
                id: node_tree_id.clone(),
            },
            EventPayload::SelectionChanged { .. } => Invalidation::Selection,
        }
    }

    /// Whether this makes the whole cache untrustworthy.
    pub fn is_total(&self) -> bool {
        matches!(self, Invalidation::Everything { .. })
    }

    /// Whether cached mesh indices survive this.
    ///
    /// Moving an object does not renumber its vertices; editing its geometry
    /// does. Conflating the two would make every transform force a re-read.
    pub fn invalidates_mesh_indices(&self) -> bool {
        matches!(
            self,
            Invalidation::MeshTopology { .. } | Invalidation::Everything { .. }
        )
    }
}

/// Which cached fields a modification touches.
///
/// Used to decide whether a cached object summary can be patched or has to be
/// re-read.
pub fn patchable(fields: &[ModifiedField]) -> bool {
    // Everything except the two that stand for "something bigger changed".
    fields
        .iter()
        .all(|field| !matches!(field, ModifiedField::Data | ModifiedField::MeshSummary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transform_invalidates_only_that_object() {
        let event = EventPayload::Modified {
            kind: AnyKind::ObjectKind,
            id: "a".into(),
            fields: vec![ModifiedField::Transform],
        };
        let invalidation = Invalidation::of(&event);
        assert_eq!(
            invalidation,
            Invalidation::Entity {
                kind: AnyKind::ObjectKind,
                id: "a".into()
            }
        );
        assert!(!invalidation.is_total());
        assert!(
            !invalidation.invalidates_mesh_indices(),
            "moving an object does not renumber its vertices"
        );
    }

    #[test]
    fn geometry_edits_invalidate_indices() {
        let event = EventPayload::MeshInvalidated {
            object_id: "a".into(),
            mesh_revision: 4,
        };
        assert!(Invalidation::of(&event).invalidates_mesh_indices());
    }

    #[test]
    fn loading_a_file_invalidates_everything_and_says_which() {
        let event = EventPayload::FileReloaded {
            filepath: Some("/tmp/x.blend".into()),
        };
        let invalidation = Invalidation::of(&event);
        assert!(invalidation.is_total());
        match invalidation {
            Invalidation::Everything { reason } => assert!(reason.contains("x.blend")),
            other => panic!("expected a total invalidation, got {other:?}"),
        }
    }

    #[test]
    fn switching_scenes_invalidates_everything() {
        let event = EventPayload::SceneChanged {
            scene_id: "s".into(),
            name: "Shot02".into(),
        };
        assert!(Invalidation::of(&event).is_total());
    }

    #[test]
    fn selection_changes_are_their_own_thing() {
        let event = EventPayload::SelectionChanged {
            selected: vec![],
            active: None,
        };
        assert_eq!(Invalidation::of(&event), Invalidation::Selection);
    }

    #[test]
    fn ordinary_fields_can_be_patched_but_data_cannot() {
        assert!(patchable(&[ModifiedField::Transform, ModifiedField::Name]));
        assert!(!patchable(&[ModifiedField::Transform, ModifiedField::Data]));
        assert!(!patchable(&[ModifiedField::MeshSummary]));
    }
}

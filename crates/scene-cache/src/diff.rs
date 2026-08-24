//! What changed between two revisions.

use blender_protocol::{event::ModifiedField, ids::AnyKind};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::revision::Revision;

/// One change to one entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Created {
        kind: AnyKind,
        id: String,
        name: String,
    },
    Deleted {
        kind: AnyKind,
        id: String,
        name: Option<String>,
    },
    Renamed {
        kind: AnyKind,
        id: String,
        from: String,
        to: String,
    },
    Modified {
        kind: AnyKind,
        id: String,
        fields: Vec<ModifiedField>,
    },
    /// A mesh's topology changed; cached indices for it are stale.
    MeshInvalidated { id: String, mesh_revision: u64 },
    /// A node graph changed too finely to describe; re-read it.
    NodeTreeInvalidated { id: String },
    SelectionChanged {
        selected: Vec<String>,
        active: Option<String>,
    },
    /// The whole scene is suspect: a file was loaded, or the scene switched.
    Reset { reason: String },
}

impl Change {
    /// The entity this change is about, when it names one.
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Change::Created { id, .. }
            | Change::Deleted { id, .. }
            | Change::Renamed { id, .. }
            | Change::Modified { id, .. }
            | Change::MeshInvalidated { id, .. }
            | Change::NodeTreeInvalidated { id } => Some(id),
            Change::SelectionChanged { .. } | Change::Reset { .. } => None,
        }
    }

    /// Whether this change makes everything earlier meaningless.
    pub fn is_reset(&self) -> bool {
        matches!(self, Change::Reset { .. })
    }
}

/// The answer to "what changed between these two points?".
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SceneDiff {
    pub from_revision: u64,
    pub to_revision: u64,
    /// Entities that appeared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<EntityChange>,
    /// Entities that changed, with which aspects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<EntityChange>,
    /// Entities that went away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<EntityChange>,
    /// Meshes whose indices are now stale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidated_meshes: Vec<String>,
    /// Node trees that must be re-read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidated_node_trees: Vec<String>,
    /// The selection at the end of the range, if it changed within it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionState>,
    /// Set when something happened that invalidates the whole cache. When this
    /// is true the lists above are not a complete picture and the caller should
    /// re-read what it cares about.
    #[serde(default)]
    pub reset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_reason: Option<String>,
}

/// One entity in a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityChange {
    pub kind: AnyKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// For renames, what it was called before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_name: Option<String>,
    /// Which cached aspects changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ModifiedField>,
}

/// Selection at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SelectionState {
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

/// Fold a sequence of changes into a diff.
///
/// The folding is what makes this useful rather than a log dump: an object
/// created and then modified five times appears once, as created. An object
/// created and then deleted within the range does not appear at all, because
/// from the caller's point of view nothing happened.
pub fn fold(from: Revision, to: Revision, changes: impl IntoIterator<Item = Change>) -> SceneDiff {
    let mut diff = SceneDiff {
        from_revision: from.value(),
        to_revision: to.value(),
        ..Default::default()
    };

    let mut created: IndexMap<String, EntityChange> = IndexMap::new();
    let mut modified: IndexMap<String, EntityChange> = IndexMap::new();
    let mut deleted: IndexMap<String, EntityChange> = IndexMap::new();
    let mut meshes: IndexMap<String, ()> = IndexMap::new();
    let mut trees: IndexMap<String, ()> = IndexMap::new();

    for change in changes {
        match change {
            Change::Reset { reason } => {
                // Everything before a reset is meaningless, so start again and
                // say so.
                created.clear();
                modified.clear();
                deleted.clear();
                meshes.clear();
                trees.clear();
                diff.reset = true;
                diff.reset_reason = Some(reason);
            }
            Change::Created { kind, id, name } => {
                deleted.shift_remove(&id);
                created.insert(
                    id.clone(),
                    EntityChange {
                        kind,
                        id,
                        name: Some(name),
                        previous_name: None,
                        fields: Vec::new(),
                    },
                );
            }
            Change::Deleted { kind, id, name } => {
                // Created then deleted inside the range: as far as anyone
                // outside is concerned it never existed.
                let was_created = created.shift_remove(&id).is_some();
                modified.shift_remove(&id);
                meshes.shift_remove(&id);
                if !was_created {
                    deleted.insert(
                        id.clone(),
                        EntityChange {
                            kind,
                            id,
                            name,
                            previous_name: None,
                            fields: Vec::new(),
                        },
                    );
                }
            }
            Change::Renamed { kind, id, from, to } => {
                if let Some(entry) = created.get_mut(&id) {
                    // Created inside the range: it simply has the new name.
                    entry.name = Some(to);
                    continue;
                }
                let entry = modified.entry(id.clone()).or_insert_with(|| EntityChange {
                    kind,
                    id,
                    name: None,
                    previous_name: Some(from),
                    fields: Vec::new(),
                });
                entry.name = Some(to);
                if !entry.fields.contains(&ModifiedField::Name) {
                    entry.fields.push(ModifiedField::Name);
                }
            }
            Change::Modified { kind, id, fields } => {
                if let Some(entry) = created.get_mut(&id) {
                    // Already reported as created; the fields are implied.
                    for field in fields {
                        if !entry.fields.contains(&field) {
                            entry.fields.push(field);
                        }
                    }
                    continue;
                }
                let entry = modified.entry(id.clone()).or_insert_with(|| EntityChange {
                    kind,
                    id,
                    name: None,
                    previous_name: None,
                    fields: Vec::new(),
                });
                for field in fields {
                    if !entry.fields.contains(&field) {
                        entry.fields.push(field);
                    }
                }
            }
            Change::MeshInvalidated { id, .. } => {
                meshes.insert(id, ());
            }
            Change::NodeTreeInvalidated { id } => {
                trees.insert(id, ());
            }
            Change::SelectionChanged { selected, active } => {
                diff.selection = Some(SelectionState { selected, active });
            }
        }
    }

    diff.created = created.into_values().collect();
    diff.modified = modified.into_values().collect();
    diff.deleted = deleted.into_values().collect();
    diff.invalidated_meshes = meshes.into_keys().collect();
    diff.invalidated_node_trees = trees.into_keys().collect();
    diff
}

impl SceneDiff {
    /// Whether anything at all changed.
    pub fn is_empty(&self) -> bool {
        !self.reset
            && self.created.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.invalidated_meshes.is_empty()
            && self.invalidated_node_trees.is_empty()
            && self.selection.is_none()
    }

    /// How many entities the diff mentions.
    pub fn len(&self) -> usize {
        self.created.len() + self.modified.len() + self.deleted.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn created(id: &str, name: &str) -> Change {
        Change::Created {
            kind: AnyKind::ObjectKind,
            id: id.into(),
            name: name.into(),
        }
    }

    fn modified(id: &str, field: ModifiedField) -> Change {
        Change::Modified {
            kind: AnyKind::ObjectKind,
            id: id.into(),
            fields: vec![field],
        }
    }

    #[test]
    fn a_creation_followed_by_edits_appears_once() {
        let diff = fold(
            Revision(0),
            Revision(3),
            [
                created("a", "Cube"),
                modified("a", ModifiedField::Transform),
                modified("a", ModifiedField::Materials),
            ],
        );
        assert_eq!(diff.created.len(), 1);
        assert!(
            diff.modified.is_empty(),
            "edits to a new object are not separate news"
        );
        assert_eq!(diff.created[0].fields.len(), 2);
    }

    #[test]
    fn create_then_delete_inside_the_range_is_a_no_op() {
        let diff = fold(
            Revision(0),
            Revision(2),
            [
                created("a", "Temp"),
                Change::Deleted {
                    kind: AnyKind::ObjectKind,
                    id: "a".into(),
                    name: Some("Temp".into()),
                },
            ],
        );
        assert!(
            diff.is_empty(),
            "nothing outside the range ever saw it: {diff:?}"
        );
    }

    #[test]
    fn delete_then_recreate_reports_a_creation() {
        let diff = fold(
            Revision(0),
            Revision(2),
            [
                Change::Deleted {
                    kind: AnyKind::ObjectKind,
                    id: "a".into(),
                    name: None,
                },
                created("a", "Cube"),
            ],
        );
        assert_eq!(diff.created.len(), 1);
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn repeated_edits_collapse_and_keep_every_field() {
        let diff = fold(
            Revision(0),
            Revision(4),
            [
                modified("a", ModifiedField::Transform),
                modified("a", ModifiedField::Transform),
                modified("a", ModifiedField::Visibility),
            ],
        );
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(
            diff.modified[0].fields,
            vec![ModifiedField::Transform, ModifiedField::Visibility]
        );
    }

    #[test]
    fn a_rename_records_both_names() {
        let diff = fold(
            Revision(0),
            Revision(1),
            [Change::Renamed {
                kind: AnyKind::ObjectKind,
                id: "a".into(),
                from: "Cube".into(),
                to: "Wall".into(),
            }],
        );
        let entry = &diff.modified[0];
        assert_eq!(entry.previous_name.as_deref(), Some("Cube"));
        assert_eq!(entry.name.as_deref(), Some("Wall"));
        assert!(entry.fields.contains(&ModifiedField::Name));
    }

    #[test]
    fn renaming_something_created_in_range_just_updates_its_name() {
        let diff = fold(
            Revision(0),
            Revision(2),
            [
                created("a", "Cube"),
                Change::Renamed {
                    kind: AnyKind::ObjectKind,
                    id: "a".into(),
                    from: "Cube".into(),
                    to: "Wall".into(),
                },
            ],
        );
        assert_eq!(diff.created.len(), 1);
        assert_eq!(diff.created[0].name.as_deref(), Some("Wall"));
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn a_reset_wipes_what_came_before_and_is_flagged() {
        let diff = fold(
            Revision(0),
            Revision(3),
            [
                created("a", "Cube"),
                Change::Reset {
                    reason: "file reloaded".into(),
                },
                created("b", "Suzanne"),
            ],
        );
        assert!(diff.reset);
        assert_eq!(diff.reset_reason.as_deref(), Some("file reloaded"));
        assert_eq!(diff.created.len(), 1, "only what came after the reset");
        assert_eq!(diff.created[0].id, "b");
    }

    #[test]
    fn invalidations_are_deduplicated() {
        let diff = fold(
            Revision(0),
            Revision(3),
            [
                Change::MeshInvalidated {
                    id: "m".into(),
                    mesh_revision: 1,
                },
                Change::MeshInvalidated {
                    id: "m".into(),
                    mesh_revision: 2,
                },
                Change::NodeTreeInvalidated { id: "t".into() },
            ],
        );
        assert_eq!(diff.invalidated_meshes, vec!["m"]);
        assert_eq!(diff.invalidated_node_trees, vec!["t"]);
    }

    #[test]
    fn deleting_something_clears_its_pending_invalidation() {
        let diff = fold(
            Revision(0),
            Revision(2),
            [
                Change::MeshInvalidated {
                    id: "m".into(),
                    mesh_revision: 1,
                },
                Change::Deleted {
                    kind: AnyKind::ObjectKind,
                    id: "m".into(),
                    name: None,
                },
            ],
        );
        assert!(
            diff.invalidated_meshes.is_empty(),
            "there is no point telling a caller to re-read something that is gone"
        );
        assert_eq!(diff.deleted.len(), 1);
    }

    #[test]
    fn only_the_last_selection_survives() {
        let diff = fold(
            Revision(0),
            Revision(2),
            [
                Change::SelectionChanged {
                    selected: vec!["a".into()],
                    active: Some("a".into()),
                },
                Change::SelectionChanged {
                    selected: vec!["b".into()],
                    active: Some("b".into()),
                },
            ],
        );
        assert_eq!(diff.selection.unwrap().selected, vec!["b"]);
    }

    #[test]
    fn an_empty_range_is_empty() {
        assert!(fold(Revision(5), Revision(5), []).is_empty());
    }
}

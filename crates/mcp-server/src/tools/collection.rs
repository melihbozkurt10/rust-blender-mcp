//! Collection tools.

use blender_protocol::{
    collection::{
        CollectionMembership, CollectionVisibility, CreateCollection, DeleteCollection,
        GetCollection, ListCollections, RenameCollection,
    },
    command::{Category, OpKind},
};

use crate::registry::ToolSpec;

const SCENE: Category = Category::Scene;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListCollections>(
            "collection.list",
            SCENE,
            OpKind::Read,
            "List collections",
            "List collections, optionally under one parent and optionally recursing into nested \
             collections. Paginated.",
        ),
        ToolSpec::forward::<GetCollection>(
            "collection.get",
            SCENE,
            OpKind::Read,
            "Get a collection",
            "One collection in detail: its objects, its children, and its viewport, render and \
             view-layer visibility.",
        ),
        ToolSpec::forward::<CreateCollection>(
            "collection.create",
            SCENE,
            OpKind::Write,
            "Create a collection",
            "Create a collection under a parent, optionally moving objects into it and giving it a \
             colour tag.",
        ),
        ToolSpec::forward::<RenameCollection>(
            "collection.rename",
            SCENE,
            OpKind::Write,
            "Rename a collection",
            "Rename a collection. Its stable id does not change.",
        ),
        ToolSpec::forward::<DeleteCollection>(
            "collection.delete",
            SCENE,
            OpKind::Write,
            "Delete a collection",
            "Delete a collection. Its objects are relinked to the parent collection unless \
             `delete_objects` is set, so nothing silently disappears from the file.",
        ),
        ToolSpec::forward::<CollectionMembership>(
            "collection.link_object",
            SCENE,
            OpKind::Write,
            "Link objects into a collection",
            "Add objects to a collection without removing them from any other.",
        ),
        ToolSpec::forward::<CollectionMembership>(
            "collection.unlink_object",
            SCENE,
            OpKind::Write,
            "Unlink objects from a collection",
            "Remove objects from a collection. Refused when it would leave an object in no \
             collection at all.",
        ),
        ToolSpec::forward::<CollectionMembership>(
            "collection.move_object",
            SCENE,
            OpKind::Write,
            "Move objects to a collection",
            "Move objects into a collection, unlinking them from the others.",
        ),
        ToolSpec::forward::<CollectionVisibility>(
            "collection.set_visibility",
            SCENE,
            OpKind::Write,
            "Set collection visibility",
            "Hide or show a collection in the viewport and renders, make it unselectable, or \
             exclude it from the view layer. Optionally recursive.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_tools_share_one_payload_shape() {
        let membership: Vec<_> = tools()
            .into_iter()
            .filter(|t| t.name.contains("_object"))
            .map(|t| serde_json::to_string(&*t.schema).unwrap())
            .collect();
        assert_eq!(membership.len(), 3);
        assert!(
            membership.windows(2).all(|pair| pair[0] == pair[1]),
            "link/unlink/move should present the same arguments"
        );
    }
}

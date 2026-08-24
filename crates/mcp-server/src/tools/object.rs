//! Object tools.

use blender_protocol::{
    command::{Category, OpKind},
    object::{
        ApplyTransforms, ClearParent, ConvertObjects, CreateObject, DeleteObjects, DisplayUpdate,
        DuplicateObjects, GetObject, JoinObjects, ListObjects, RenameObject, SeparateObject,
        SetOrigin, SetParent, TransformObject, VisibilityUpdate,
    },
};

use crate::registry::ToolSpec;

/// Short alias so the table below stays readable.
const SCENE: Category = Category::Scene;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListObjects>(
            "object.list",
            Category::Core,
            OpKind::Read,
            "List objects",
            "List objects with optional filters on name, type, collection, selection, visibility, \
             material and modifiers. Paginated: pass `limit` and the returned `next_cursor`.",
        ),
        ToolSpec::forward::<GetObject>(
            "object.get",
            Category::Core,
            OpKind::Read,
            "Get an object",
            "Full detail for one object: transform, hierarchy, collections, materials, modifiers, \
             constraints, mesh counts and custom properties.",
        ),
        ToolSpec::forward::<CreateObject>(
            "object.create",
            SCENE,
            OpKind::Write,
            "Create an object",
            "Create a primitive, empty, curve, text, camera or light. Location, rotation, scale or \
             target dimensions, and the destination collection can all be set in the same call.",
        ),
        ToolSpec::forward::<DeleteObjects>(
            "object.delete",
            SCENE,
            OpKind::Write,
            "Delete objects",
            "Delete objects, optionally with their descendants and their orphaned data-blocks.",
        ),
        ToolSpec::forward::<DuplicateObjects>(
            "object.duplicate",
            SCENE,
            OpKind::Write,
            "Duplicate objects",
            "Copy objects, optionally sharing mesh data, several times, with a per-copy offset. \
             Each copy gets a fresh stable id.",
        ),
        ToolSpec::forward::<RenameObject>(
            "object.rename",
            SCENE,
            OpKind::Write,
            "Rename an object",
            "Rename an object, and optionally its data-block. The stable id is unchanged.",
        ),
        ToolSpec::forward::<TransformObject>(
            "object.transform",
            SCENE,
            OpKind::Write,
            "Transform an object",
            "Set or offset location, rotation, scale or world-space dimensions. Rotation accepts \
             radians, degrees or a quaternion.",
        ),
        ToolSpec::forward::<ApplyTransforms>(
            "object.transform.apply",
            SCENE,
            OpKind::Write,
            "Apply transforms",
            "Bake location, rotation or scale into the mesh data, resetting the object transform. \
             Refused on objects that share their data with others.",
        ),
        ToolSpec::forward::<SetParent>(
            "object.set_parent",
            SCENE,
            OpKind::Write,
            "Parent an object",
            "Parent one object to another, to a bone, or via an armature deform. Cycles are \
             refused. The child keeps its world transform unless told otherwise.",
        ),
        ToolSpec::forward::<ClearParent>(
            "object.clear_parent",
            SCENE,
            OpKind::Write,
            "Clear parenting",
            "Unparent objects, keeping their world transform by default.",
        ),
        ToolSpec::forward::<VisibilityUpdate>(
            "object.hide",
            SCENE,
            OpKind::Write,
            "Hide objects",
            "Hide objects in the viewport, in renders, or both.",
        ),
        ToolSpec::forward::<VisibilityUpdate>(
            "object.show",
            SCENE,
            OpKind::Write,
            "Show objects",
            "Un-hide objects in the viewport, in renders, or both.",
        ),
        ToolSpec::forward::<DisplayUpdate>(
            "object.set_display",
            SCENE,
            OpKind::Write,
            "Set viewport display",
            "Change how objects draw in the viewport: display type, wireframe overlay, in-front \
             drawing, name label and object colour.",
        ),
        ToolSpec::forward::<JoinObjects>(
            "object.join",
            SCENE,
            OpKind::Write,
            "Join objects",
            "Merge several objects of the same type into one. The sources are consumed.",
        ),
        ToolSpec::forward::<SeparateObject>(
            "object.separate",
            SCENE,
            OpKind::Write,
            "Separate a mesh",
            "Split a mesh into several objects by loose parts, by material, or by the current \
             selection.",
        ),
        ToolSpec::forward::<SetOrigin>(
            "object.origin.set",
            SCENE,
            OpKind::Write,
            "Set object origin",
            "Move an object origin to its geometry, its bounds centre or bottom, the 3D cursor, \
             its centre of mass, or an explicit world point.",
        ),
        ToolSpec::forward::<ConvertObjects>(
            "object.convert",
            SCENE,
            OpKind::Write,
            "Convert object type",
            "Convert curves, text and other convertible objects to mesh, or mesh to curve.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_and_getting_are_core_tools() {
        // These two are the model's way into a scene, so they must be
        // available before any category has been enabled.
        for tool in tools() {
            if matches!(tool.name, "object.list" | "object.get") {
                assert_eq!(tool.category, Category::Core, "{}", tool.name);
                assert_eq!(tool.kind, OpKind::Read);
            }
        }
    }

    #[test]
    fn every_object_tool_is_namespaced() {
        for tool in tools() {
            assert!(tool.name.starts_with("object."), "{}", tool.name);
        }
    }
}

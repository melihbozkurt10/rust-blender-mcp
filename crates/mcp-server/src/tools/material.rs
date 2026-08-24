//! Material tools.

use blender_protocol::{
    command::{Category, OpKind},
    material::{
        AssignMaterial, CreateMaterial, DeleteMaterial, DuplicateMaterial, GetMaterial,
        ListMaterialSlots, ListMaterials, UnassignMaterial, UpdateMaterial,
    },
};

use crate::registry::ToolSpec;

const MATERIALS: Category = Category::Materials;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListMaterials>(
            "material.list",
            MATERIALS,
            OpKind::Read,
            "List materials",
            "List materials, optionally filtered by name, by the object using them, or by whether \
             they have any users at all. Paginated.",
        ),
        ToolSpec::forward::<GetMaterial>(
            "material.get",
            MATERIALS,
            OpKind::Read,
            "Get a material",
            "One material in detail: its Principled BSDF values, blend and display settings, node \
             count and the image files it references.",
        ),
        ToolSpec::forward::<CreateMaterial>(
            "material.create",
            MATERIALS,
            OpKind::Write,
            "Create a material",
            "Create a material, set its Principled BSDF values, and optionally assign it to \
             objects in the same call.",
        ),
        ToolSpec::forward::<UpdateMaterial>(
            "material.update",
            MATERIALS,
            OpKind::Write,
            "Update a material",
            "Change a material name, its Principled BSDF values, or its blend and display \
             settings. Socket names that moved between Blender versions are handled for you.",
        ),
        ToolSpec::forward::<DuplicateMaterial>(
            "material.duplicate",
            MATERIALS,
            OpKind::Write,
            "Duplicate a material",
            "Copy a material, including its whole node graph. The copy gets a fresh stable id.",
        ),
        ToolSpec::forward::<DeleteMaterial>(
            "material.delete",
            MATERIALS,
            OpKind::Write,
            "Delete a material",
            "Delete a material. Refused while other data-blocks still use it, unless forced.",
        ),
        ToolSpec::forward::<AssignMaterial>(
            "material.assign",
            MATERIALS,
            OpKind::Write,
            "Assign a material",
            "Assign a material to objects, into a specific slot, replacing every slot, or onto \
             particular faces. Face assignment checks the mesh revision so stale indices are \
             refused rather than applied to the wrong faces.",
        ),
        ToolSpec::forward::<UnassignMaterial>(
            "material.unassign",
            MATERIALS,
            OpKind::Write,
            "Unassign a material",
            "Clear material slots on objects, optionally only those holding one particular \
             material, and optionally removing the emptied slot.",
        ),
        ToolSpec::forward::<ListMaterialSlots>(
            "material.slot.list",
            MATERIALS,
            OpKind::Read,
            "List material slots",
            "The material slots on one object, in order, with what each holds.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_documents_the_staleness_check() {
        let assign = tools()
            .into_iter()
            .find(|t| t.name == "material.assign")
            .unwrap();
        assert!(
            assign.description.contains("mesh revision"),
            "the face-index staleness rule needs to be discoverable from the description"
        );
    }
}

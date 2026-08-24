//! Rigging and rig diagnostics tools.

use blender_protocol::{
    command::{Category, OpKind},
    rig::{
        ArmatureRefParams, BindMesh, BoneRefParams, ConstraintOperation, ConstraintRefParams,
        CreateArmature, CreateBone, FixNaming, ListBones, ListConstraints, MirrorBones,
        NormalizeWeights, ParentBone, SymmetryDiagnostics, UpdateBone, VertexGroupListParams,
        VertexGroupOperation, WeightDiagnostics,
    },
};

use super::NoParams;
use crate::registry::ToolSpec;

const RIG: Category = Category::Rigging;
const DIAGNOSTICS: Category = Category::RigDiagnostics;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "rig.armature.list",
            RIG,
            OpKind::Read,
            "List armatures",
            "Every armature, with its bone counts, root bones and bound meshes.",
        ),
        ToolSpec::forward::<ArmatureRefParams>(
            "rig.armature.get",
            RIG,
            OpKind::Read,
            "Get an armature",
            "One armature with every bone.",
        ),
        ToolSpec::forward::<CreateArmature>(
            "rig.armature.create",
            RIG,
            OpKind::Write,
            "Create an armature",
            "Create an armature and, optionally, its whole bone hierarchy in one call. Parents \
             must be listed before their children, and zero-length bones are refused rather than \
             silently discarded by Blender.",
        ),
        ToolSpec::forward::<ListBones>(
            "rig.bone.list",
            RIG,
            OpKind::Read,
            "List bones",
            "Bones in an armature, optionally only deforming ones or only the children of one \
             bone. Paginated.",
        ),
        ToolSpec::forward::<BoneRefParams>(
            "rig.bone.get",
            RIG,
            OpKind::Read,
            "Get a bone",
            "One bone with its head, tail, roll, parent, children and constraints.",
        ),
        ToolSpec::forward::<CreateBone>(
            "rig.bone.create",
            RIG,
            OpKind::Write,
            "Create a bone",
            "Add one bone to an existing armature.",
        ),
        ToolSpec::forward::<UpdateBone>(
            "rig.bone.update",
            RIG,
            OpKind::Write,
            "Update a bone",
            "Move a bone's head or tail, change its roll, rename it, or stop it deforming.",
        ),
        ToolSpec::forward::<BoneRefParams>(
            "rig.bone.delete",
            RIG,
            OpKind::Write,
            "Delete a bone",
            "Remove a bone from an armature.",
        ),
        ToolSpec::forward::<ParentBone>(
            "rig.bone.parent",
            RIG,
            OpKind::Write,
            "Parent a bone",
            "Set or clear a bone's parent, optionally connecting it. Cycles are refused.",
        ),
        ToolSpec::forward::<MirrorBones>(
            "rig.bone.mirror",
            RIG,
            OpKind::Write,
            "Mirror bones",
            "Copy bones from one side to the other using a naming convention, mirroring position \
             and roll. Existing bones on the far side are kept unless `overwrite` is set, and \
             `dry_run` reports what would happen first.",
        ),
        ToolSpec::forward::<VertexGroupListParams>(
            "rig.vertex_group.list",
            RIG,
            OpKind::Read,
            "List vertex groups",
            "The vertex groups on a mesh, with their indices and lock state.",
        ),
        ToolSpec::forward::<VertexGroupOperation>(
            "rig.vertex_group.create",
            RIG,
            OpKind::Write,
            "Create a vertex group",
            "Add an empty vertex group to a mesh.",
        ),
        ToolSpec::forward::<VertexGroupOperation>(
            "rig.vertex_group.delete",
            RIG,
            OpKind::Write,
            "Delete a vertex group",
            "Remove a vertex group and its weights.",
        ),
        ToolSpec::forward::<VertexGroupOperation>(
            "rig.vertex_group.assign",
            RIG,
            OpKind::Write,
            "Assign weights",
            "Set, add to or subtract from the weight of named vertices in a group.",
        ),
        ToolSpec::forward::<NormalizeWeights>(
            "rig.vertex_group.normalize",
            RIG,
            OpKind::Write,
            "Normalise weights",
            "Make each vertex's deform weights sum to 1, optionally capping how many bones may \
             influence one vertex -- game engines usually allow four.",
        ),
        ToolSpec::forward::<BindMesh>(
            "rig.parent_mesh",
            RIG,
            OpKind::Write,
            "Bind meshes to an armature",
            "Parent meshes to an armature with automatic, envelope or empty weights.",
        ),
        ToolSpec::forward::<BindMesh>(
            "rig.auto_weights",
            RIG,
            OpKind::Write,
            "Bind with automatic weights",
            "Bind meshes to an armature using heat-map weights. The same operation as \
             `rig.parent_mesh` with automatic weighting, under the name people look for.",
        ),
        ToolSpec::forward::<ListConstraints>(
            "rig.constraint.list",
            RIG,
            OpKind::Read,
            "List constraints",
            "Constraints on an object or on one of its pose bones.",
        ),
        ToolSpec::forward::<ConstraintOperation>(
            "rig.constraint.add",
            RIG,
            OpKind::Write,
            "Add a constraint",
            "Add a copy-transform, tracking, IK, limit or child-of constraint to an object or pose \
             bone.",
        ),
        ToolSpec::forward::<ConstraintRefParams>(
            "rig.constraint.update",
            RIG,
            OpKind::Write,
            "Update a constraint",
            "Change a constraint's target, influence, chain length or type-specific properties.",
        ),
        ToolSpec::forward::<ConstraintRefParams>(
            "rig.constraint.remove",
            RIG,
            OpKind::Write,
            "Remove a constraint",
            "Delete a constraint from an object or pose bone.",
        ),
        // -- diagnostics -----------------------------------------------------
        ToolSpec::forward::<ArmatureRefParams>(
            "rig.diagnostics.health",
            DIAGNOSTICS,
            OpKind::Read,
            "Check rig health",
            "Find zero-length bones, broken connections, orphaned vertex groups, deform bones with \
             no weights and constraints pointing at nothing. Every finding carries a code, the \
             entity it concerns and a suggested fix.",
        ),
        ToolSpec::forward::<SymmetryDiagnostics>(
            "rig.diagnostics.naming",
            DIAGNOSTICS,
            OpKind::Read,
            "Check bone naming",
            "Detect the left/right convention in use and report bones that carry a side marker \
             with no counterpart, mixed conventions, and Blender collision suffixes.",
        ),
        ToolSpec::forward::<WeightDiagnostics>(
            "rig.diagnostics.weights",
            DIAGNOSTICS,
            OpKind::Read,
            "Check weights",
            "Find unweighted vertices, weights that do not sum to 1, weights outside 0..1, \
             vertices influenced by too many bones, and empty vertex groups.",
        ),
        ToolSpec::forward::<SymmetryDiagnostics>(
            "rig.diagnostics.symmetry",
            DIAGNOSTICS,
            OpKind::Read,
            "Check symmetry",
            "Compare left and right bone pairs and report where they have drifted apart.",
        ),
        ToolSpec::forward::<FixNaming>(
            "rig.fix.naming",
            DIAGNOSTICS,
            OpKind::Write,
            "Normalise bone naming",
            "Convert bone names to one left/right convention, renaming the matching vertex groups \
             at the same time -- renaming bones without them silently breaks deformation. Defaults \
             to a dry run.",
        ),
        ToolSpec::forward::<NormalizeWeights>(
            "rig.fix.normalize_weights",
            DIAGNOSTICS,
            OpKind::Write,
            "Fix weights",
            "Normalise deform weights and optionally cap influences per vertex. Supports a dry run.",
        ),
        ToolSpec::forward::<MirrorBones>(
            "rig.fix.mirror_bones",
            DIAGNOSTICS,
            OpKind::Write,
            "Fix symmetry",
            "Re-mirror bones from one side to the other to repair asymmetry. Supports a dry run.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_only_read_and_fixes_only_write() {
        for tool in tools() {
            if tool.name.starts_with("rig.diagnostics.") {
                assert_eq!(tool.kind, OpKind::Read, "{}", tool.name);
                assert_eq!(tool.category, DIAGNOSTICS, "{}", tool.name);
            }
            if tool.name.starts_with("rig.fix.") {
                assert_eq!(tool.kind, OpKind::Write, "{}", tool.name);
            }
        }
    }

    #[test]
    fn every_fix_offers_a_dry_run() {
        for tool in tools()
            .into_iter()
            .filter(|t| t.name.starts_with("rig.fix."))
        {
            let schema = serde_json::to_string(&*tool.schema).unwrap();
            assert!(schema.contains("dry_run"), "`{}` has no dry run", tool.name);
        }
    }

    #[test]
    fn renaming_warns_about_vertex_groups() {
        let fix = tools()
            .into_iter()
            .find(|t| t.name == "rig.fix.naming")
            .unwrap();
        assert!(fix.description.contains("vertex groups"));
    }
}

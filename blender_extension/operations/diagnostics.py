"""Rig and scene diagnostics.

Findings carry a stable code, the entity they concern, and a suggested fix, so
a model can act on them without parsing prose. Nothing here changes anything;
the `rig.fix.*` operations do, and every one of them supports a dry run.
"""

from __future__ import annotations

import re
from typing import Any

import bpy

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from .rigging import (
    SIDE_CONVENTIONS,
    armature_arg,
    detect_convention,
    mirror_name,
    require_armature_object,
)

INFO, WARNING, ERROR = "INFO", "WARNING", "ERROR"


def finding(
    severity: str, code: str, message: str, entity: str | None = None, fix: str | None = None, **details: Any
) -> dict[str, Any]:
    payload: dict[str, Any] = {"severity": severity, "code": code, "message": message}
    if entity is not None:
        payload["entity"] = entity
    if fix is not None:
        payload["suggested_fix"] = fix
    if details:
        payload["details"] = details
    return payload


def _bound_meshes(armature_object) -> list:
    return [
        obj
        for obj in bpy.data.objects
        if obj.type == "MESH"
        and any(m.type == "ARMATURE" and m.object == armature_object for m in obj.modifiers)
    ]


def _summarise(findings: list[dict[str, Any]]) -> str | None:
    order = {INFO: 0, WARNING: 1, ERROR: 2}
    if not findings:
        return None
    return max(findings, key=lambda f: order[f["severity"]])["severity"]


@read("rig.diagnostics.health")
def health(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    armature = obj.data
    meshes = _bound_meshes(obj)
    findings: list[dict[str, Any]] = []

    bone_names = {bone.name for bone in armature.bones}
    deform_bones = {bone.name for bone in armature.bones if bone.use_deform}

    for bone in armature.bones:
        if bone.length < 1e-5:
            findings.append(
                finding(
                    ERROR,
                    "ZERO_LENGTH_BONE",
                    f"Bone `{bone.name}` has effectively zero length and will be discarded.",
                    entity=bone.name,
                    fix="Move its tail with rig.bone.update.",
                    length=float(bone.length),
                )
            )
        if bone.parent is not None and bone.use_connect:
            gap = (bone.head_local - bone.parent.tail_local).length
            if gap > 1e-4:
                findings.append(
                    finding(
                        WARNING,
                        "BROKEN_CONNECTION",
                        f"Bone `{bone.name}` is marked connected but sits {gap:.4f} away from its "
                        "parent tail.",
                        entity=bone.name,
                        fix="Re-parent with rig.bone.parent, or move the head.",
                        gap=float(gap),
                    )
                )

    if not deform_bones:
        findings.append(
            finding(
                WARNING,
                "NO_DEFORM_BONES",
                "No bone in this armature deforms geometry, so binding a mesh would do nothing.",
                entity=obj.name,
                fix="Set deform on the bones that should move the mesh.",
            )
        )

    if not meshes:
        findings.append(
            finding(
                INFO,
                "NO_BOUND_MESHES",
                "No mesh is bound to this armature.",
                entity=obj.name,
                fix="Bind one with rig.parent_mesh.",
            )
        )

    for mesh_object in meshes:
        group_names = {group.name for group in mesh_object.vertex_groups}
        orphaned = sorted(group_names - bone_names)
        if orphaned:
            findings.append(
                finding(
                    WARNING,
                    "ORPHANED_VERTEX_GROUPS",
                    f"`{mesh_object.name}` has {len(orphaned)} vertex group(s) with no matching "
                    "bone; they deform nothing.",
                    entity=mesh_object.name,
                    fix="Delete them with rig.vertex_group.delete, or rename the bones.",
                    groups=orphaned[:20],
                )
            )
        missing = sorted(deform_bones - group_names)
        if missing:
            findings.append(
                finding(
                    INFO,
                    "BONES_WITHOUT_WEIGHTS",
                    f"{len(missing)} deform bone(s) have no vertex group on `{mesh_object.name}`.",
                    entity=mesh_object.name,
                    fix="Re-run rig.auto_weights, or paint the missing groups.",
                    bones=missing[:20],
                )
            )

    # Constraint targets that have gone missing produce a rig that silently
    # stops working, and the UI does not shout about it.
    for pose_bone in obj.pose.bones:
        for constraint in pose_bone.constraints:
            target = getattr(constraint, "target", None)
            if hasattr(constraint, "target") and target is None:
                findings.append(
                    finding(
                        ERROR,
                        "CONSTRAINT_TARGET_MISSING",
                        f"Constraint `{constraint.name}` on bone `{pose_bone.name}` has no target.",
                        entity=f"{pose_bone.name}/{constraint.name}",
                        fix="Set a target with rig.constraint.update, or remove the constraint.",
                        constraint_type=constraint.type,
                    )
                )
            subtarget = getattr(constraint, "subtarget", "")
            if subtarget and target is not None and getattr(target, "type", "") == "ARMATURE":
                if subtarget not in target.data.bones:
                    findings.append(
                        finding(
                            ERROR,
                            "CONSTRAINT_SUBTARGET_MISSING",
                            f"Constraint `{constraint.name}` points at bone `{subtarget}`, which "
                            f"`{target.name}` does not have.",
                            entity=f"{pose_bone.name}/{constraint.name}",
                            fix="Correct the subtarget with rig.constraint.update.",
                        )
                    )

    return {
        "armature": ids.ensure_id(obj),
        "findings": findings,
        "bone_count": len(armature.bones),
        "deform_bone_count": len(deform_bones),
        "bound_meshes": [mesh.name for mesh in meshes],
        "worst_severity": _summarise(findings),
        "revision": ctx.revision,
    }


@read("rig.diagnostics.naming")
def naming(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    names = [bone.name for bone in obj.data.bones]
    requested = c.optional_str(args, "convention")
    convention = (
        c.enum_value(requested, sorted(SIDE_CONVENTIONS), "convention")
        if requested
        else detect_convention(names)
    )

    findings: list[dict[str, Any]] = []
    if convention is None:
        findings.append(
            finding(
                INFO,
                "NO_SIDE_CONVENTION",
                "No bone name carries a left/right marker, so symmetry tools have nothing to work "
                "with.",
                entity=obj.name,
                fix="Rename paired bones with rig.fix.naming, choosing a convention.",
            )
        )
        return {
            "armature": ids.ensure_id(obj),
            "convention": None,
            "findings": findings,
            "worst_severity": _summarise(findings),
            "revision": ctx.revision,
        }

    mismatched = []
    for name in names:
        mirrored = mirror_name(name, convention)
        if mirrored is None:
            continue
        if mirrored not in names:
            mismatched.append({"bone": name, "expected_pair": mirrored})

    if mismatched:
        findings.append(
            finding(
                WARNING,
                "UNPAIRED_SIDE_BONES",
                f"{len(mismatched)} bone(s) carry a side marker but have no counterpart.",
                entity=obj.name,
                fix="Mirror them with rig.bone.mirror, or correct the names.",
                bones=mismatched[:20],
            )
        )

    # Blender appends `.001` when a name collides; those are almost always
    # accidents in a rig.
    duplicates = [name for name in names if re.search(r"\.\d{3}$", name)]
    if duplicates:
        findings.append(
            finding(
                WARNING,
                "DUPLICATE_SUFFIX",
                f"{len(duplicates)} bone(s) end in a Blender collision suffix.",
                entity=obj.name,
                fix="Rename them with rig.bone.update.",
                bones=duplicates[:20],
            )
        )

    others = {
        other
        for other in SIDE_CONVENTIONS
        if other != convention and detect_convention(names) == other
    }
    if others:
        findings.append(
            finding(
                WARNING,
                "MIXED_CONVENTIONS",
                "Bone names mix more than one left/right convention.",
                entity=obj.name,
                fix="Normalise with rig.fix.naming.",
            )
        )

    return {
        "armature": ids.ensure_id(obj),
        "convention": convention,
        "findings": findings,
        "worst_severity": _summarise(findings),
        "revision": ctx.revision,
    }


@read("rig.diagnostics.weights")
def weights(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    tolerance = c.optional_float(args, "tolerance", 0.001) or 0.001
    max_influences = c.optional_int(args, "max_influences")
    sample_limit = c.optional_int(args, "sample_limit", 50) or 50

    findings: list[dict[str, Any]] = []
    statistics = []

    for obj in objects:
        mesh = c.require_mesh(obj)
        group_names = {group.index: group.name for group in obj.vertex_groups}
        if not group_names:
            findings.append(
                finding(
                    WARNING,
                    "NO_VERTEX_GROUPS",
                    f"`{obj.name}` has no vertex groups, so an armature cannot deform it.",
                    entity=obj.name,
                    fix="Bind it with rig.parent_mesh.",
                )
            )
            continue

        unweighted: list[int] = []
        unnormalised: list[dict[str, Any]] = []
        over_influenced: list[dict[str, Any]] = []
        out_of_range: list[dict[str, Any]] = []
        used_groups: set[int] = set()

        for vertex in mesh.vertices:
            entries = list(vertex.groups)
            if not entries:
                if len(unweighted) < sample_limit:
                    unweighted.append(vertex.index)
                continue
            total = 0.0
            for entry in entries:
                used_groups.add(entry.group)
                total += entry.weight
                if entry.weight < 0.0 or entry.weight > 1.0:
                    if len(out_of_range) < sample_limit:
                        out_of_range.append(
                            {"vertex": vertex.index, "weight": float(entry.weight)}
                        )
            if abs(total - 1.0) > tolerance and len(unnormalised) < sample_limit:
                unnormalised.append({"vertex": vertex.index, "total": float(total)})
            if max_influences is not None and len(entries) > max_influences:
                if len(over_influenced) < sample_limit:
                    over_influenced.append(
                        {"vertex": vertex.index, "influences": len(entries)}
                    )

        if unweighted:
            findings.append(
                finding(
                    ERROR,
                    "UNWEIGHTED_VERTICES",
                    f"`{obj.name}` has vertices with no weights; they will not follow the rig.",
                    entity=obj.name,
                    fix="Re-run rig.auto_weights, or assign them manually.",
                    sample=unweighted,
                )
            )
        if unnormalised:
            findings.append(
                finding(
                    WARNING,
                    "UNNORMALISED_WEIGHTS",
                    f"`{obj.name}` has vertices whose weights do not sum to 1.",
                    entity=obj.name,
                    fix="Run rig.fix.normalize_weights.",
                    sample=unnormalised,
                )
            )
        if out_of_range:
            findings.append(
                finding(
                    ERROR,
                    "WEIGHTS_OUT_OF_RANGE",
                    f"`{obj.name}` has weights outside 0..1.",
                    entity=obj.name,
                    fix="Run rig.fix.normalize_weights.",
                    sample=out_of_range,
                )
            )
        if over_influenced:
            findings.append(
                finding(
                    WARNING,
                    "TOO_MANY_INFLUENCES",
                    f"`{obj.name}` has vertices influenced by more than {max_influences} bones, "
                    "which most game engines will not import.",
                    entity=obj.name,
                    fix=f"Run rig.fix.normalize_weights with max_influences={max_influences}.",
                    sample=over_influenced,
                )
            )

        empty_groups = sorted(
            name for index, name in group_names.items() if index not in used_groups
        )
        if empty_groups:
            findings.append(
                finding(
                    INFO,
                    "EMPTY_VERTEX_GROUPS",
                    f"`{obj.name}` has {len(empty_groups)} vertex group(s) with no weighted "
                    "vertices.",
                    entity=obj.name,
                    fix="Delete them with rig.vertex_group.delete.",
                    groups=empty_groups[:20],
                )
            )

        statistics.append(
            {
                "object": ids.ensure_id(obj),
                "name": obj.name,
                "vertices": len(mesh.vertices),
                "vertex_groups": len(group_names),
                "unweighted": len(unweighted),
                "unnormalised": len(unnormalised),
            }
        )

    return {
        "findings": findings,
        "objects": statistics,
        "worst_severity": _summarise(findings),
        "revision": ctx.revision,
    }


@read("rig.diagnostics.symmetry")
def symmetry(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    axis = c.enum_value(c.optional_str(args, "axis", "X") or "X", ["X", "Y", "Z"], "axis")
    tolerance = c.optional_float(args, "tolerance", 0.0001) or 0.0001
    component = {"X": 0, "Y": 1, "Z": 2}[axis]

    names = [bone.name for bone in obj.data.bones]
    requested = c.optional_str(args, "convention")
    convention = (
        c.enum_value(requested, sorted(SIDE_CONVENTIONS), "convention")
        if requested
        else detect_convention(names)
    )

    findings: list[dict[str, Any]] = []
    if convention is None:
        findings.append(
            finding(
                INFO,
                "NO_SIDE_CONVENTION",
                "Without a left/right naming convention there are no pairs to compare.",
                entity=obj.name,
                fix="Name paired bones with rig.fix.naming first.",
            )
        )
        return {
            "armature": ids.ensure_id(obj),
            "findings": findings,
            "worst_severity": _summarise(findings),
            "revision": ctx.revision,
        }

    compared = 0
    for bone in obj.data.bones:
        partner_name = mirror_name(bone.name, convention)
        if partner_name is None or partner_name not in obj.data.bones:
            continue
        if bone.name > partner_name:
            continue
        partner = obj.data.bones[partner_name]
        compared += 1

        for label, ours, theirs in (
            ("head", bone.head_local, partner.head_local),
            ("tail", bone.tail_local, partner.tail_local),
        ):
            expected = list(theirs)
            expected[component] = -expected[component]
            drift = max(abs(ours[i] - expected[i]) for i in range(3))
            if drift > tolerance:
                findings.append(
                    finding(
                        WARNING,
                        "ASYMMETRIC_BONE",
                        f"`{bone.name}` and `{partner_name}` differ by {drift:.5f} at the {label}.",
                        entity=bone.name,
                        fix="Re-mirror with rig.bone.mirror and overwrite:true.",
                        drift=float(drift),
                        partner=partner_name,
                    )
                )

        if abs(bone.length - partner.length) > tolerance:
            findings.append(
                finding(
                    WARNING,
                    "ASYMMETRIC_LENGTH",
                    f"`{bone.name}` and `{partner_name}` have different lengths.",
                    entity=bone.name,
                    fix="Re-mirror with rig.bone.mirror and overwrite:true.",
                    partner=partner_name,
                )
            )

    return {
        "armature": ids.ensure_id(obj),
        "convention": convention,
        "pairs_compared": compared,
        "findings": findings,
        "worst_severity": _summarise(findings),
        "revision": ctx.revision,
    }


@op("rig.fix.naming")
def fix_naming(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    convention = c.enum_value(
        c.optional_str(args, "convention", "DOT_SUFFIX") or "DOT_SUFFIX",
        sorted(SIDE_CONVENTIONS),
        "convention",
    )
    rename_groups = c.optional_bool(args, "rename_vertex_groups", True)
    dry_run = c.optional_bool(args, "dry_run", True)

    target_left, target_right, target_is_suffix = SIDE_CONVENTIONS[convention]
    renames: list[dict[str, Any]] = []

    for bone in obj.data.bones:
        for source, (left, right, is_suffix) in SIDE_CONVENTIONS.items():
            if source == convention:
                continue
            side = None
            stem = bone.name
            if is_suffix and bone.name.endswith(left):
                side, stem = "L", bone.name[: -len(left)]
            elif is_suffix and bone.name.endswith(right):
                side, stem = "R", bone.name[: -len(right)]
            elif not is_suffix and bone.name.startswith(left):
                side, stem = "L", bone.name[len(left) :]
            elif not is_suffix and bone.name.startswith(right):
                side, stem = "R", bone.name[len(right) :]
            if side is None:
                continue
            marker = target_left if side == "L" else target_right
            new_name = f"{stem}{marker}" if target_is_suffix else f"{marker}{stem}"
            if new_name != bone.name:
                renames.append(
                    {"from": bone.name, "to": new_name, "applied": False, "reason": f"from {source}"}
                )
            break

    if dry_run:
        return {
            "armature": ids.ensure_id(obj),
            "convention": convention,
            "dry_run": True,
            "renames": renames,
            "revision": ctx.revision,
        }

    meshes = _bound_meshes(obj) if rename_groups else []
    for entry in renames:
        bone = obj.data.bones.get(entry["from"])
        if bone is None:
            continue
        bone.name = entry["to"]
        entry["applied"] = True
        # Renaming a bone without renaming its vertex group silently breaks
        # deformation, so the two move together by default.
        for mesh_object in meshes:
            group = mesh_object.vertex_groups.get(entry["from"])
            if group is not None:
                group.name = entry["to"]

    ctx.bump()
    return {
        "armature": ids.ensure_id(obj),
        "convention": convention,
        "dry_run": False,
        "renames": renames,
        "vertex_groups_renamed": bool(rename_groups),
        "revision": ctx.revision,
    }


@op("rig.fix.normalize_weights")
def fix_normalize_weights(ctx, args: dict) -> dict[str, Any]:
    from .rigging import normalize_weights

    return normalize_weights(ctx, args)


@op("rig.fix.mirror_bones")
def fix_mirror_bones(ctx, args: dict) -> dict[str, Any]:
    from .rigging import mirror_bones

    return mirror_bones(ctx, args)


# --- scene-level diagnostics ----------------------------------------------


@read("scene.find_missing_textures")
def find_missing_textures(ctx, args: dict) -> dict[str, Any]:
    missing = []
    for image in bpy.data.images:
        if image.source not in {"FILE", "SEQUENCE", "MOVIE", "TILED"}:
            continue
        if image.packed_file is not None:
            continue
        if not image.filepath:
            continue
        if not image.has_data:
            missing.append(
                {
                    "id": ids.ensure_id(image),
                    "name": image.name,
                    "filepath": image.filepath,
                    "users": int(image.users),
                    "used_by": _image_users(image),
                }
            )
    return {"missing": missing, "total": len(missing), "revision": ctx.revision}


def _image_users(image) -> list[str]:
    users = []
    for material in bpy.data.materials:
        if not material.use_nodes or material.node_tree is None:
            continue
        if any(getattr(node, "image", None) == image for node in material.node_tree.nodes):
            users.append(material.name)
    return users[:20]


@read("scene.find_duplicates")
def find_duplicates(ctx, args: dict) -> dict[str, Any]:
    """Find objects and materials that look like accidental copies."""
    tolerance = c.optional_float(args, "tolerance", 0.0001) or 0.0001

    by_signature: dict[tuple, list] = {}
    for obj in bpy.context.scene.objects:
        if obj.type != "MESH" or obj.data is None:
            continue
        mesh = obj.data
        signature = (
            len(mesh.vertices),
            len(mesh.edges),
            len(mesh.polygons),
            round(obj.location.x / tolerance),
            round(obj.location.y / tolerance),
            round(obj.location.z / tolerance),
        )
        by_signature.setdefault(signature, []).append(obj)

    overlapping = [
        {
            "objects": [{"id": ids.ensure_id(o), "name": o.name} for o in group],
            "location": c.vector_dict(group[0].location),
            "reason": "same geometry counts at the same location",
        }
        for group in by_signature.values()
        if len(group) > 1
    ]

    material_groups: dict[str, list] = {}
    for material in bpy.data.materials:
        stem = re.sub(r"\.\d{3}$", "", material.name)
        material_groups.setdefault(stem, []).append(material)
    duplicate_materials = [
        {
            "stem": stem,
            "materials": [{"id": ids.ensure_id(m), "name": m.name, "users": m.users} for m in group],
        }
        for stem, group in material_groups.items()
        if len(group) > 1
    ]

    linked_meshes = [
        {"mesh": mesh.name, "users": mesh.users}
        for mesh in bpy.data.meshes
        if mesh.users > 1
    ]

    return {
        "overlapping_objects": overlapping,
        "duplicate_materials": duplicate_materials,
        "shared_meshes": linked_meshes,
        "revision": ctx.revision,
    }

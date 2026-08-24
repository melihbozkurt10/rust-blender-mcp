"""Armatures, bones, vertex groups, weights and constraints."""

from __future__ import annotations

from typing import Any

import bpy
from mathutils import Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

SIDE_CONVENTIONS = {
    "DOT_SUFFIX": (".L", ".R", True),
    "UNDERSCORE_SUFFIX": ("_L", "_R", True),
    "WORD_PREFIX": ("Left ", "Right ", False),
    "CAMEL_PREFIX": ("Left", "Right", False),
}


def mirror_name(name: str, convention: str) -> str | None:
    """The opposite-side name, or ``None`` when the name carries no side."""
    left, right, is_suffix = SIDE_CONVENTIONS[convention]
    if is_suffix:
        if name.endswith(left):
            return name[: -len(left)] + right
        if name.endswith(right):
            return name[: -len(right)] + left
    else:
        if name.startswith(left):
            return right + name[len(left) :]
        if name.startswith(right):
            return left + name[len(right) :]
    return None


def detect_convention(names: list[str]) -> str | None:
    """Which side convention a set of bone names appears to follow."""
    scores = {}
    for convention, (left, right, is_suffix) in SIDE_CONVENTIONS.items():
        if is_suffix:
            hits = sum(1 for n in names if n.endswith(left) or n.endswith(right))
        else:
            hits = sum(1 for n in names if n.startswith(left) or n.startswith(right))
        scores[convention] = hits
    best = max(scores, key=lambda k: scores[k])
    return best if scores[best] > 0 else None


def require_armature_object(reference: str):
    obj = ids.find_object(reference)
    if obj.type != "ARMATURE":
        raise BridgeError(
            ErrorCode.ARMATURE_NOT_FOUND,
            f"`{obj.name}` is a {obj.type} object, not an armature.",
            {"object": obj.name, "type": obj.type},
        )
    return obj


def armature_arg(args: dict, key: str = "armature"):
    return require_armature_object(c.require_str(args, key))


def bone_id(armature_object, bone) -> str:
    """A bone stable id.

    Bones are not ID data-blocks, but they do accept custom properties, so the
    same UUID scheme used for objects works here.
    """
    from .. import config

    existing = bone.get(config.ID_PROPERTY)
    if isinstance(existing, str) and existing:
        return existing
    import uuid

    new_id = str(uuid.uuid4())
    try:
        bone[config.ID_PROPERTY] = new_id
    except TypeError:
        # Some bone types reject custom properties; fall back to the name,
        # which is unique within an armature.
        return bone.name
    return new_id


def find_bone(armature_object, reference: str):
    bones = armature_object.data.bones
    for bone in bones:
        if bone.get(ids.config.ID_PROPERTY) == reference:
            return bone
    bone = bones.get(reference)
    if bone is not None:
        return bone
    raise BridgeError(
        ErrorCode.BONE_NOT_FOUND,
        f"`{armature_object.name}` has no bone `{reference}`.",
        {
            "armature": armature_object.name,
            "reference": reference,
            "available": [b.name for b in bones][:40],
        },
    )


def summarise_bone(armature_object, bone, *, detail: bool = False) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "id": bone_id(armature_object, bone),
        "name": bone.name,
        "head": c.vector_dict(bone.head_local),
        "tail": c.vector_dict(bone.tail_local),
        "length": float(bone.length),
        "parent": bone.parent.name if bone.parent else None,
        "connected": bool(bone.use_connect),
        "deform": bool(bone.use_deform),
    }
    if detail:
        payload["children"] = [child.name for child in bone.children]
        pose_bone = armature_object.pose.bones.get(bone.name)
        if pose_bone is not None:
            payload["constraints"] = [
                {"name": con.name, "type": con.type} for con in pose_bone.constraints
            ]
    return payload


def summarise_armature(obj, *, detail: bool = False) -> dict[str, Any]:
    armature = obj.data
    bound = [
        mesh.name
        for mesh in bpy.data.objects
        if mesh.type == "MESH"
        and any(m.type == "ARMATURE" and m.object == obj for m in mesh.modifiers)
    ]
    payload: dict[str, Any] = {
        "id": ids.ensure_id(obj),
        "data_id": ids.ensure_id(armature),
        "name": obj.name,
        "bone_count": len(armature.bones),
        "deform_bone_count": sum(1 for bone in armature.bones if bone.use_deform),
        "root_bones": [bone.name for bone in armature.bones if bone.parent is None],
        "bound_meshes": bound,
        "action": (
            obj.animation_data.action.name
            if obj.animation_data and obj.animation_data.action
            else None
        ),
    }
    if detail:
        payload["bones"] = [summarise_bone(obj, bone) for bone in armature.bones]
    return payload


# --- armatures -------------------------------------------------------------


@read("rig.armature.list")
def list_armatures(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    matched = [
        obj
        for obj in bpy.data.objects
        if obj.type == "ARMATURE" and c.matches_name(obj.name, name_filter)
    ]
    matched.sort(key=lambda o: o.name)
    window, cursor = c.paginate(matched, args)
    return {
        "armatures": [summarise_armature(obj) for obj in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("rig.armature.get")
def get_armature(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    return {"armature": summarise_armature(obj, detail=True), "revision": ctx.revision}


@op("rig.armature.create")
def create_armature(ctx, args: dict) -> dict[str, Any]:
    name = c.optional_str(args, "name") or "Armature"
    location = c.optional_vector(args, "location") or Vector((0.0, 0.0, 0.0))

    armature = bpy.data.armatures.new(name)
    obj = bpy.data.objects.new(name, armature)
    obj.location = location
    bpy.context.scene.collection.objects.link(obj)

    if c.optional_bool(args, "show_names", False):
        armature.show_names = True
    display_type = c.optional_str(args, "display_type")
    if display_type is not None:
        armature.display_type = c.enum_value(
            display_type, ["OCTAHEDRAL", "STICK", "BBONE", "ENVELOPE", "WIRE"], "display_type"
        )

    specs = c.optional_list(args, "bones")
    if specs:
        _create_bones(obj, specs)

    ids.invalidate_cache("object")
    ctx.bump()
    return {"armature": summarise_armature(obj, detail=True), "revision": ctx.revision}


def _create_bones(armature_object, specs: list[dict]) -> list[str]:
    """Create bones in edit mode, parents before children."""
    created: list[str] = []
    with c.object_mode(armature_object, "EDIT"):
        edit_bones = armature_object.data.edit_bones
        for spec in specs:
            name = str(spec["name"])
            head = c.as_vector(spec["head"], "head")
            tail = c.as_vector(spec["tail"], "tail")
            if (tail - head).length < 1e-6:
                raise invalid_argument(
                    f"Bone `{name}` has zero length; Blender discards such bones.",
                    bone=name,
                )
            if name in edit_bones:
                raise invalid_argument(f"Bone `{name}` already exists.", bone=name)

            bone = edit_bones.new(name)
            bone.head = head
            bone.tail = tail
            bone.roll = float(spec.get("roll", 0.0))
            bone.use_deform = bool(spec.get("deform", True))

            parent_name = spec.get("parent")
            if parent_name is not None:
                parent = edit_bones.get(str(parent_name))
                if parent is None:
                    raise invalid_argument(
                        f"Bone `{name}` names parent `{parent_name}`, which does not exist yet. "
                        "List parents before their children.",
                        bone=name,
                        parent=str(parent_name),
                    )
                bone.parent = parent
                bone.use_connect = bool(spec.get("connected", False))
            elif spec.get("connected"):
                raise invalid_argument(
                    f"Bone `{name}` is `connected` but has no parent.", bone=name
                )
            created.append(name)
    return created


@op("rig.bone.create")
def create_bone(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    spec = {
        "name": c.require_str(args, "name"),
        "head": c.require(args, "head"),
        "tail": c.require(args, "tail"),
        "parent": c.optional_str(args, "parent"),
        "connected": c.optional_bool(args, "connected", False),
        "roll": c.optional_float(args, "roll", 0.0),
        "deform": c.optional_bool(args, "deform", True),
    }
    _create_bones(obj, [spec])
    ctx.bump()
    bone = find_bone(obj, spec["name"])
    return {
        "armature": ids.ensure_id(obj),
        "bone": summarise_bone(obj, bone, detail=True),
        "revision": ctx.revision,
    }


@read("rig.bone.list")
def list_bones(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    name_filter = c.optional_str(args, "name_contains")
    deform_only = c.optional_bool(args, "deform_only")
    parent = c.optional_str(args, "parent")

    bones = list(obj.data.bones)
    if name_filter is not None:
        bones = [b for b in bones if c.matches_name(b.name, name_filter)]
    if deform_only is not None:
        bones = [b for b in bones if bool(b.use_deform) == deform_only]
    if parent is not None:
        bones = [b for b in bones if b.parent is not None and b.parent.name == parent]

    window, cursor = c.paginate(bones, args)
    return {
        "armature": ids.ensure_id(obj),
        "bones": [summarise_bone(obj, bone) for bone in window],
        "total": len(bones),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("rig.bone.get")
def get_bone(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    bone = find_bone(obj, c.require_str(args, "bone"))
    return {
        "armature": ids.ensure_id(obj),
        "bone": summarise_bone(obj, bone, detail=True),
        "revision": ctx.revision,
    }


@op("rig.bone.update")
def update_bone(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    reference = c.require_str(args, "bone")
    bone = find_bone(obj, reference)
    original_name = bone.name
    changed: list[str] = []

    with c.object_mode(obj, "EDIT"):
        edit_bone = obj.data.edit_bones[original_name]
        head = c.optional_vector(args, "head")
        tail = c.optional_vector(args, "tail")
        if head is not None:
            edit_bone.head = head
            changed.append("head")
        if tail is not None:
            edit_bone.tail = tail
            changed.append("tail")
        if (edit_bone.tail - edit_bone.head).length < 1e-6:
            raise invalid_argument(
                f"That would give `{original_name}` zero length, and Blender would discard it.",
                bone=original_name,
            )
        roll = c.optional_float(args, "roll")
        if roll is not None:
            edit_bone.roll = roll
            changed.append("roll")
        deform = c.optional_bool(args, "deform")
        if deform is not None:
            edit_bone.use_deform = deform
            changed.append("deform")
        name = c.optional_str(args, "name")
        if name is not None:
            edit_bone.name = name
            changed.append("name")

    if not changed:
        raise invalid_argument("Nothing to update on this bone.")

    ctx.bump()
    bone = find_bone(obj, c.optional_str(args, "name") or original_name)
    return {
        "bone": summarise_bone(obj, bone, detail=True),
        "changed": changed,
        "revision": ctx.revision,
    }


@op("rig.bone.delete")
def delete_bone(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    bone = find_bone(obj, c.require_str(args, "bone"))
    name = bone.name
    with c.object_mode(obj, "EDIT"):
        edit_bone = obj.data.edit_bones.get(name)
        if edit_bone is not None:
            obj.data.edit_bones.remove(edit_bone)
    ctx.bump()
    return {"armature": ids.ensure_id(obj), "removed": name, "revision": ctx.revision}


@op("rig.bone.parent")
def parent_bone(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    child_name = find_bone(obj, c.require_str(args, "bone")).name
    parent_reference = c.optional_str(args, "parent")
    connected = c.optional_bool(args, "connected", False)

    parent_name = find_bone(obj, parent_reference).name if parent_reference else None
    if parent_name == child_name:
        raise invalid_argument("A bone cannot be its own parent.")

    with c.object_mode(obj, "EDIT"):
        edit_bones = obj.data.edit_bones
        child = edit_bones[child_name]
        if parent_name is None:
            child.parent = None
            child.use_connect = False
        else:
            if _would_cycle(edit_bones[parent_name], child_name):
                raise invalid_argument(
                    f"`{parent_name}` is already a descendant of `{child_name}`; parenting them "
                    "would make a cycle.",
                    bone=child_name,
                    parent=parent_name,
                )
            child.parent = edit_bones[parent_name]
            child.use_connect = bool(connected)

    ctx.bump()
    return {
        "bone": summarise_bone(obj, find_bone(obj, child_name), detail=True),
        "revision": ctx.revision,
    }


def _would_cycle(candidate_parent, child_name: str) -> bool:
    current = candidate_parent
    while current is not None:
        if current.name == child_name:
            return True
        current = current.parent
    return False


@op("rig.bone.mirror")
def mirror_bones(ctx, args: dict) -> dict[str, Any]:
    obj = armature_arg(args)
    convention = c.enum_value(
        c.optional_str(args, "convention", "DOT_SUFFIX") or "DOT_SUFFIX",
        sorted(SIDE_CONVENTIONS),
        "convention",
    )
    direction = c.enum_value(
        c.optional_str(args, "direction", "LEFT_TO_RIGHT") or "LEFT_TO_RIGHT",
        ["LEFT_TO_RIGHT", "RIGHT_TO_LEFT"],
        "direction",
    )
    axis = c.enum_value(c.optional_str(args, "axis", "X") or "X", ["X", "Y", "Z"], "axis")
    overwrite = c.optional_bool(args, "overwrite", False)
    dry_run = c.optional_bool(args, "dry_run", False)
    wanted = {str(name) for name in c.optional_list(args, "bones")}

    left, right, is_suffix = SIDE_CONVENTIONS[convention]
    source_marker = left if direction == "LEFT_TO_RIGHT" else right
    component = {"X": 0, "Y": 1, "Z": 2}[axis]

    planned: list[dict[str, Any]] = []
    for bone in obj.data.bones:
        if wanted and bone.name not in wanted:
            continue
        matches = bone.name.endswith(source_marker) if is_suffix else bone.name.startswith(source_marker)
        if not matches:
            continue
        target = mirror_name(bone.name, convention)
        if target is None:
            continue
        exists = target in obj.data.bones
        if exists and not overwrite:
            planned.append({"from": bone.name, "to": target, "skipped": "already exists"})
            continue
        planned.append({"from": bone.name, "to": target, "skipped": None})

    if dry_run:
        return {"armature": ids.ensure_id(obj), "planned": planned, "revision": ctx.revision}

    created = []
    with c.object_mode(obj, "EDIT"):
        edit_bones = obj.data.edit_bones
        for entry in planned:
            if entry["skipped"]:
                continue
            source = edit_bones[entry["from"]]
            existing = edit_bones.get(entry["to"])
            if existing is not None:
                edit_bones.remove(existing)
            mirrored = edit_bones.new(entry["to"])
            head, tail = list(source.head), list(source.tail)
            head[component] = -head[component]
            tail[component] = -tail[component]
            mirrored.head = head
            mirrored.tail = tail
            mirrored.roll = -source.roll
            mirrored.use_deform = source.use_deform
            if source.parent is not None:
                parent_mirror = mirror_name(source.parent.name, convention)
                parent = edit_bones.get(parent_mirror) if parent_mirror else None
                mirrored.parent = parent or source.parent
                mirrored.use_connect = source.use_connect
            created.append(entry["to"])

    ctx.bump()
    return {
        "armature": ids.ensure_id(obj),
        "created": created,
        "planned": planned,
        "revision": ctx.revision,
    }


# --- vertex groups ---------------------------------------------------------


@read("rig.vertex_group.list")
def list_vertex_groups(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    groups = [
        {"name": group.name, "index": group.index, "lock_weight": bool(group.lock_weight)}
        for group in obj.vertex_groups
    ]
    return {
        "object": ids.ensure_id(obj),
        "vertex_groups": groups,
        "total": len(groups),
        "revision": ctx.revision,
    }


@op("rig.vertex_group.create")
def create_vertex_group(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.require_str(args, "group")
    if obj.vertex_groups.get(name) is not None:
        raise invalid_argument(
            f"`{obj.name}` already has a vertex group `{name}`.", object=obj.name
        )
    group = obj.vertex_groups.new(name=name)
    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "group": {"name": group.name, "index": group.index},
        "revision": ctx.revision,
    }


@op("rig.vertex_group.delete")
def delete_vertex_group(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.require_str(args, "group")
    group = obj.vertex_groups.get(name)
    if group is None:
        raise invalid_argument(
            f"`{obj.name}` has no vertex group `{name}`.",
            available=[g.name for g in obj.vertex_groups],
        )
    obj.vertex_groups.remove(group)
    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": name, "revision": ctx.revision}


@op("rig.vertex_group.assign")
def assign_vertex_group(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    name = c.require_str(args, "group")
    vertices = [int(i) for i in c.optional_list(args, "vertices")]
    weight = c.optional_float(args, "weight", 1.0)
    mode = c.enum_value(
        c.optional_str(args, "mode", "REPLACE") or "REPLACE",
        ["REPLACE", "ADD", "SUBTRACT"],
        "mode",
    )
    ids.check_mesh_revision(mesh, c.optional_int(args, "expected_mesh_revision"))

    if not vertices:
        raise invalid_argument("`vertices` must not be empty.", field="vertices")
    count = len(mesh.vertices)
    bad = [i for i in vertices if i < 0 or i >= count]
    if bad:
        raise invalid_argument(
            f"`{obj.name}` has {count} vertices; {len(bad)} indices are out of range.",
            out_of_range=bad[:20],
            vertex_count=count,
        )

    group = obj.vertex_groups.get(name) or obj.vertex_groups.new(name=name)
    group.add(vertices, float(weight), mode)
    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "group": group.name,
        "vertices": len(vertices),
        "weight": float(weight),
        "revision": ctx.revision,
    }


@op("rig.vertex_group.normalize")
def normalize_weights(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    wanted = {str(name) for name in c.optional_list(args, "groups")}
    max_influences = c.optional_int(args, "max_influences")
    dry_run = c.optional_bool(args, "dry_run", False)

    report = []
    for obj in objects:
        mesh = c.require_mesh(obj)
        deform_indices = _deform_group_indices(obj, wanted)
        if not deform_indices:
            report.append({"object": ids.ensure_id(obj), "skipped": "no deform vertex groups"})
            continue

        adjusted = trimmed = 0
        for vertex in mesh.vertices:
            entries = [g for g in vertex.groups if g.group in deform_indices]
            if not entries:
                continue

            if max_influences is not None and len(entries) > max_influences:
                entries.sort(key=lambda g: g.weight, reverse=True)
                for extra in entries[max_influences:]:
                    if not dry_run:
                        obj.vertex_groups[extra.group].remove([vertex.index])
                    trimmed += 1
                entries = entries[:max_influences]

            total = sum(entry.weight for entry in entries)
            if total <= 0.0 or abs(total - 1.0) < 1e-6:
                continue
            adjusted += 1
            if dry_run:
                continue
            for entry in entries:
                obj.vertex_groups[entry.group].add(
                    [vertex.index], entry.weight / total, "REPLACE"
                )

        report.append(
            {
                "object": ids.ensure_id(obj),
                "name": obj.name,
                "vertices_adjusted": adjusted,
                "influences_trimmed": trimmed,
                "groups": len(deform_indices),
            }
        )

    if not dry_run:
        ctx.bump()
    return {"dry_run": bool(dry_run), "objects": report, "revision": ctx.revision}


def _deform_group_indices(obj, wanted: set[str]) -> set[int]:
    """Which vertex groups correspond to deforming bones."""
    bone_names: set[str] = set()
    for modifier in obj.modifiers:
        if modifier.type == "ARMATURE" and modifier.object is not None:
            bone_names |= {b.name for b in modifier.object.data.bones if b.use_deform}

    indices = set()
    for group in obj.vertex_groups:
        if wanted and group.name not in wanted:
            continue
        if bone_names and group.name not in bone_names:
            continue
        if group.lock_weight:
            continue
        indices.add(group.index)
    return indices


# --- binding ---------------------------------------------------------------


@op("rig.parent_mesh")
def parent_mesh(ctx, args: dict) -> dict[str, Any]:
    obj = require_armature_object(c.require_str(args, "armature"))
    meshes = c.objects_arg(args, "meshes")
    weighting = c.enum_value(
        c.optional_str(args, "weighting", "AUTOMATIC") or "AUTOMATIC",
        ["AUTOMATIC", "ENVELOPE", "EMPTY"],
        "weighting",
    )
    keep_existing = c.optional_bool(args, "keep_existing_groups", False)

    if obj in meshes:
        raise invalid_argument("The armature cannot also be one of the meshes.")
    for mesh_object in meshes:
        c.require_mesh(mesh_object)

    parent_type = {
        "AUTOMATIC": "ARMATURE_AUTO",
        "ENVELOPE": "ARMATURE_ENVELOPE",
        "EMPTY": "ARMATURE_NAME",
    }[weighting]

    view_layer = bpy.context.view_layer
    with c.object_mode(obj):
        for mesh_object in meshes:
            if mesh_object.name not in view_layer.objects:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"`{mesh_object.name}` is not in the active view layer and cannot be bound.",
                    {"object": mesh_object.name},
                )
            if not keep_existing:
                for group in list(mesh_object.vertex_groups):
                    mesh_object.vertex_groups.remove(group)
            mesh_object.select_set(True)
        view_layer.objects.active = obj
        try:
            bpy.ops.object.parent_set(type=parent_type)
        except RuntimeError as error:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"Binding failed: {error}",
                {"armature": obj.name, "weighting": weighting},
            ) from error

    ctx.bump()
    return {
        "armature": ids.ensure_id(obj),
        "meshes": [
            {
                "id": ids.ensure_id(mesh_object),
                "name": mesh_object.name,
                "vertex_groups": len(mesh_object.vertex_groups),
            }
            for mesh_object in meshes
        ],
        "weighting": weighting,
        "revision": ctx.revision,
    }


@op("rig.auto_weights")
def auto_weights(ctx, args: dict) -> dict[str, Any]:
    payload = dict(args)
    payload["weighting"] = "AUTOMATIC"
    return parent_mesh(ctx, payload)


# --- constraints -----------------------------------------------------------


def _constraint_holder(obj, bone_name: str | None):
    if bone_name is None:
        return obj, obj.constraints
    if obj.type != "ARMATURE":
        raise invalid_argument(f"`{obj.name}` is not an armature.", object=obj.name)
    pose_bone = obj.pose.bones.get(bone_name)
    if pose_bone is None:
        raise BridgeError(
            ErrorCode.BONE_NOT_FOUND,
            f"`{obj.name}` has no bone `{bone_name}`.",
            {"armature": obj.name, "available": [b.name for b in obj.pose.bones][:40]},
        )
    return pose_bone, pose_bone.constraints


@read("rig.constraint.list")
def list_constraints(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    _holder, constraints = _constraint_holder(obj, c.optional_str(args, "bone"))
    return {
        "object": ids.ensure_id(obj),
        "bone": c.optional_str(args, "bone"),
        "constraints": [
            {
                "name": con.name,
                "type": con.type,
                "influence": float(getattr(con, "influence", 1.0)),
                "target": getattr(getattr(con, "target", None), "name", None),
                "subtarget": getattr(con, "subtarget", None) or None,
                "muted": bool(con.mute),
            }
            for con in constraints
        ],
        "total": len(constraints),
        "revision": ctx.revision,
    }


@op("rig.constraint.add")
def add_constraint(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    bone_name = c.optional_str(args, "bone")
    _holder, constraints = _constraint_holder(obj, bone_name)
    constraint_type = c.require_str(args, "type")

    try:
        constraint = constraints.new(type=constraint_type)
    except (RuntimeError, TypeError) as error:
        available = [
            item.identifier
            for item in bpy.types.Constraint.bl_rna.properties["type"].enum_items
        ]
        raise BridgeError(
            ErrorCode.CAPABILITY_UNAVAILABLE,
            f"`{constraint_type}` is not a constraint type here: {error}",
            {"requested": constraint_type, "available": available},
        ) from error

    _apply_constraint(constraint, args)
    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "bone": bone_name,
        "constraint": {"name": constraint.name, "type": constraint.type},
        "revision": ctx.revision,
    }


def _apply_constraint(constraint, args: dict) -> list[str]:
    from . import _nodes as n

    changed: list[str] = []
    name = c.optional_str(args, "name")
    if name is not None:
        constraint.name = name
        changed.append("name")

    target_reference = c.optional_str(args, "target")
    if target_reference is not None:
        if not hasattr(constraint, "target"):
            raise invalid_argument(
                f"A {constraint.type} constraint has no target.", constraint_type=constraint.type
            )
        constraint.target = ids.find_object(target_reference)
        changed.append("target")

    subtarget = c.optional_str(args, "subtarget")
    if subtarget is not None:
        if not hasattr(constraint, "subtarget"):
            raise invalid_argument(
                f"A {constraint.type} constraint has no subtarget.",
                constraint_type=constraint.type,
            )
        constraint.subtarget = subtarget
        changed.append("subtarget")

    influence = c.optional_float(args, "influence")
    if influence is not None and hasattr(constraint, "influence"):
        constraint.influence = influence
        changed.append("influence")

    chain = c.optional_int(args, "chain_length")
    if chain is not None:
        if not hasattr(constraint, "chain_count"):
            raise invalid_argument(
                f"A {constraint.type} constraint has no chain length.",
                constraint_type=constraint.type,
            )
        constraint.chain_count = chain
        changed.append("chain_length")

    for assignment in c.optional_list(args, "properties"):
        property_name = str(assignment["name"])
        rna = constraint.bl_rna.properties.get(property_name)
        if rna is None or rna.is_readonly:
            raise BridgeError(
                ErrorCode.INVALID_PROPERTY,
                f"A {constraint.type} constraint has no writable property `{property_name}`.",
                {
                    "constraint_type": constraint.type,
                    "available": sorted(
                        p.identifier for p in constraint.bl_rna.properties if not p.is_readonly
                    ),
                },
            )
        setattr(constraint, property_name, n.decode_value(assignment["value"], property_name))
        changed.append(f"property:{property_name}")

    return changed


@op("rig.constraint.update")
def update_constraint(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    bone_name = c.optional_str(args, "bone")
    _holder, constraints = _constraint_holder(obj, bone_name)
    name = c.require_str(args, "constraint")
    constraint = constraints.get(name)
    if constraint is None:
        raise invalid_argument(
            f"No constraint named `{name}`.",
            available=[con.name for con in constraints],
        )
    changed = _apply_constraint(constraint, args)
    if not changed:
        raise invalid_argument("Nothing to update on this constraint.")
    ctx.bump()
    return {"constraint": {"name": constraint.name, "type": constraint.type}, "changed": changed, "revision": ctx.revision}


@op("rig.constraint.remove")
def remove_constraint(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    bone_name = c.optional_str(args, "bone")
    _holder, constraints = _constraint_holder(obj, bone_name)
    name = c.require_str(args, "constraint")
    constraint = constraints.get(name)
    if constraint is None:
        raise invalid_argument(
            f"No constraint named `{name}`.",
            available=[con.name for con in constraints],
        )
    constraints.remove(constraint)
    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": name, "revision": ctx.revision}

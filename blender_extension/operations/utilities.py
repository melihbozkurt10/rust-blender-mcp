"""Scene housekeeping: cleanup, batch rename, orphan purging, transforms.

Every destructive pass is opt-in and every one supports a dry run. A single
`cleanup: true` that quietly deletes data is exactly the kind of operation that
loses somebody an afternoon.
"""

from __future__ import annotations

import re
from typing import Any

import bmesh
import bpy

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

CLEANUP_PASSES = (
    "purge_orphans",
    "remove_empty_collections",
    "remove_loose_geometry",
    "remove_unused_material_slots",
    "merge_duplicate_materials",
    "recalculate_normals",
    "remove_invalid_modifiers",
)


@op("scene.cleanup")
def cleanup(ctx, args: dict) -> dict[str, Any]:
    enabled = {name: bool(c.optional_bool(args, name, False)) for name in CLEANUP_PASSES}
    dry_run = bool(c.optional_bool(args, "dry_run", False))

    if not any(enabled.values()):
        raise invalid_argument(
            "`scene.cleanup` does nothing unless a pass is enabled. Set the passes you want "
            "explicitly.",
            available=list(CLEANUP_PASSES),
        )

    report: dict[str, Any] = {}

    if enabled["remove_loose_geometry"]:
        report["remove_loose_geometry"] = _remove_loose(dry_run)
    if enabled["remove_unused_material_slots"]:
        report["remove_unused_material_slots"] = _remove_unused_slots(dry_run)
    if enabled["merge_duplicate_materials"]:
        report["merge_duplicate_materials"] = _merge_materials(dry_run)
    if enabled["recalculate_normals"]:
        report["recalculate_normals"] = _recalculate_normals(dry_run)
    if enabled["remove_invalid_modifiers"]:
        report["remove_invalid_modifiers"] = _remove_invalid_modifiers(dry_run)
    if enabled["remove_empty_collections"]:
        report["remove_empty_collections"] = _remove_empty_collections(dry_run)
    # Orphan purging runs last: the passes above are what turn data-blocks into
    # orphans in the first place.
    if enabled["purge_orphans"]:
        report["purge_orphans"] = _purge_orphans(dry_run)

    if not dry_run:
        ids.invalidate_cache()
        ctx.bump()

    return {"dry_run": dry_run, "passes": report, "revision": ctx.revision}


def _remove_loose(dry_run: bool) -> dict[str, Any]:
    affected = []
    for obj in bpy.context.scene.objects:
        if obj.type != "MESH" or obj.data is None or obj.data.users > 1:
            continue
        mesh = obj.data
        bm = bmesh.new()
        bm.from_mesh(mesh)
        loose_verts = [v for v in bm.verts if not v.link_edges]
        loose_edges = [e for e in bm.edges if not e.link_faces]
        if loose_verts or loose_edges:
            affected.append(
                {
                    "object": obj.name,
                    "loose_vertices": len(loose_verts),
                    "loose_edges": len(loose_edges),
                }
            )
            if not dry_run:
                bmesh.ops.delete(bm, geom=loose_edges, context="EDGES")
                bmesh.ops.delete(bm, geom=loose_verts, context="VERTS")
                bm.to_mesh(mesh)
                mesh.update()
                ids.next_mesh_revision(mesh)
        bm.free()
    return {"objects": affected, "count": len(affected)}


def _remove_unused_slots(dry_run: bool) -> dict[str, Any]:
    affected = []
    for obj in bpy.context.scene.objects:
        if not hasattr(obj.data, "materials"):
            continue
        empty = [index for index, slot in enumerate(obj.material_slots) if slot.material is None]
        if not empty:
            continue
        affected.append({"object": obj.name, "slots": empty})
        if not dry_run:
            for index in sorted(empty, reverse=True):
                obj.data.materials.pop(index=index)
    return {"objects": affected, "count": len(affected)}


def _merge_materials(dry_run: bool) -> dict[str, Any]:
    """Merge materials whose names differ only by a Blender collision suffix.

    Only when their settings match: two materials called `Wood` and `Wood.001`
    can be genuinely different, and merging those would be destructive in a way
    the caller did not ask for.
    """
    groups: dict[str, list] = {}
    for material in bpy.data.materials:
        stem = re.sub(r"\.\d{3}$", "", material.name)
        groups.setdefault(stem, []).append(material)

    merged = []
    for stem, group in groups.items():
        if len(group) < 2:
            continue
        group.sort(key=lambda m: m.name)
        keeper = group[0]
        for duplicate in group[1:]:
            if not _materials_match(keeper, duplicate):
                continue
            merged.append({"kept": keeper.name, "merged": duplicate.name})
            if not dry_run:
                duplicate.user_remap(keeper)
                bpy.data.materials.remove(duplicate)
    return {"merges": merged, "count": len(merged)}


def _materials_match(first, second) -> bool:
    from .material import read_principled

    if first.use_nodes != second.use_nodes:
        return False
    left, right = read_principled(first), read_principled(second)
    if left is None or right is None:
        return left == right
    if set(left) != set(right):
        return False
    for key, value in left.items():
        other = right[key]
        if isinstance(value, dict) and isinstance(other, dict):
            if any(abs(value[k] - other.get(k, 0.0)) > 1e-4 for k in value):
                return False
        elif isinstance(value, (int, float)) and abs(value - other) > 1e-4:
            return False
    return True


def _recalculate_normals(dry_run: bool) -> dict[str, Any]:
    affected = []
    for obj in bpy.context.scene.objects:
        if obj.type != "MESH" or obj.data is None or obj.data.users > 1:
            continue
        affected.append(obj.name)
        if not dry_run:
            bm = bmesh.new()
            bm.from_mesh(obj.data)
            bmesh.ops.recalc_face_normals(bm, faces=list(bm.faces))
            bm.to_mesh(obj.data)
            obj.data.update()
            bm.free()
    return {"objects": affected, "count": len(affected)}


def _remove_invalid_modifiers(dry_run: bool) -> dict[str, Any]:
    from .modifier import TARGET_PROPERTY

    removed = []
    needs_target = {"BOOLEAN", "CURVE", "HOOK", "LATTICE", "SHRINKWRAP"}
    for obj in bpy.context.scene.objects:
        for modifier in list(obj.modifiers):
            if modifier.type not in needs_target:
                continue
            attribute = TARGET_PROPERTY.get(modifier.type)
            if attribute is None:
                continue
            if getattr(modifier, attribute, None) is None:
                removed.append(
                    {"object": obj.name, "modifier": modifier.name, "type": modifier.type}
                )
                if not dry_run:
                    obj.modifiers.remove(modifier)
    return {"modifiers": removed, "count": len(removed)}


def _remove_empty_collections(dry_run: bool) -> dict[str, Any]:
    removed = []
    # Repeat until stable: emptying a child can empty its parent.
    while True:
        candidates = [
            collection
            for collection in bpy.data.collections
            if not collection.objects and not collection.children
        ]
        if not candidates:
            break
        for collection in candidates:
            removed.append(collection.name)
            if not dry_run:
                bpy.data.collections.remove(collection)
        if dry_run:
            break
    return {"collections": removed, "count": len(removed)}


def _purge_orphans(dry_run: bool) -> dict[str, Any]:
    if dry_run:
        counts = {}
        for name in ("meshes", "materials", "images", "actions", "node_groups", "textures", "curves", "armatures"):
            collection = getattr(bpy.data, name, None)
            if collection is None:
                continue
            orphans = [item.name for item in collection if item.users == 0]
            if orphans:
                counts[name] = orphans
        return {"would_remove": counts, "count": sum(len(v) for v in counts.values())}

    # Blender's own purge handles indirect users and repeats until stable.
    removed = bpy.data.orphans_purge(do_local_ids=True, do_linked_ids=True, do_recursive=True)
    return {"removed": int(removed)}


@op("scene.purge_orphans")
def purge_orphans(ctx, args: dict) -> dict[str, Any]:
    dry_run = bool(c.optional_bool(args, "dry_run", False))
    result = _purge_orphans(dry_run)
    if not dry_run:
        ids.invalidate_cache()
        ctx.bump()
    return {"dry_run": dry_run, **result, "revision": ctx.revision}


@op("scene.batch_rename")
def batch_rename(ctx, args: dict) -> dict[str, Any]:
    """Rename many data-blocks with one pattern.

    The regular expression is compiled here, in Python, against a caller string
    -- but `re` is a pattern matcher, not an evaluator, so the worst a bad
    pattern can do is fail to compile, which is reported as an invalid argument.
    A catastrophic-backtracking pattern is the real risk, so the input length is
    capped and matching is done on short names.
    """
    kind = c.enum_value(
        c.optional_str(args, "kind", "objects") or "objects",
        ["objects", "materials", "collections", "meshes", "actions", "images"],
        "kind",
    )
    dry_run = bool(c.optional_bool(args, "dry_run", False))
    targets = _rename_targets(kind, args)

    find = c.optional_str(args, "find")
    replace = c.optional_str(args, "replace")
    pattern_text = c.optional_str(args, "regex")
    prefix = c.optional_str(args, "prefix")
    suffix = c.optional_str(args, "suffix")
    strip_start = c.optional_int(args, "strip_start")
    strip_end = c.optional_int(args, "strip_end")
    case = c.optional_str(args, "case")
    number_start = c.optional_int(args, "number_start")
    number_padding = c.optional_int(args, "number_padding", 3) or 3
    number_position = c.optional_str(args, "number_position", "SUFFIX") or "SUFFIX"

    if find is not None and pattern_text is not None:
        raise invalid_argument("Set `find` or `regex`, not both.")
    if (find is not None or pattern_text is not None) and replace is None:
        raise invalid_argument("`replace` is required alongside `find` or `regex`.")

    pattern = None
    if pattern_text is not None:
        if len(pattern_text) > 200:
            raise invalid_argument("`regex` is too long.", field="regex")
        try:
            pattern = re.compile(pattern_text)
        except re.error as error:
            raise invalid_argument(
                f"`regex` did not compile: {error}", field="regex"
            ) from error

    if case is not None:
        case = c.enum_value(case, ["UPPER", "LOWER", "TITLE"], "case")
    if number_position is not None:
        number_position = c.enum_value(number_position, ["PREFIX", "SUFFIX"], "number_position")

    renames = []
    counter = number_start
    for item in targets:
        name = item.name
        if find is not None:
            name = name.replace(find, replace)
        elif pattern is not None:
            name = pattern.sub(replace, name)
        if strip_start:
            name = name[strip_start:]
        if strip_end:
            name = name[: -strip_end] if strip_end < len(name) else ""
        if prefix:
            name = f"{prefix}{name}"
        if suffix:
            name = f"{name}{suffix}"
        if case == "UPPER":
            name = name.upper()
        elif case == "LOWER":
            name = name.lower()
        elif case == "TITLE":
            name = name.title()
        if counter is not None:
            number = str(counter).zfill(number_padding)
            name = f"{number}{name}" if number_position == "PREFIX" else f"{name}{number}"
            counter += 1

        name = name.strip()
        if not name:
            raise invalid_argument(
                f"Renaming `{item.name}` would produce an empty name.", entity=item.name
            )
        if name == item.name:
            continue
        renames.append({"from": item.name, "to": name, "applied": False})

    if not dry_run:
        for entry in renames:
            item = _find_by_name(kind, entry["from"])
            if item is None:
                continue
            item.name = entry["to"]
            entry["applied"] = True
        ids.invalidate_cache()
        ctx.bump()

    return {
        "kind": kind,
        "dry_run": dry_run,
        "renames": renames,
        "count": len(renames),
        "revision": ctx.revision,
    }


def _rename_targets(kind: str, args: dict) -> list:
    collection = {
        "objects": bpy.data.objects,
        "materials": bpy.data.materials,
        "collections": bpy.data.collections,
        "meshes": bpy.data.meshes,
        "actions": bpy.data.actions,
        "images": bpy.data.images,
    }[kind]

    explicit = c.optional_list(args, "targets")
    if explicit:
        found = []
        for reference in explicit:
            item = collection.get(str(reference))
            if item is None and kind == "objects":
                item = ids.find_object(str(reference))
            if item is None:
                raise invalid_argument(f"No {kind[:-1]} matches `{reference}`.")
            found.append(item)
        return found

    name_filter = c.optional_str(args, "name_contains")
    items = [item for item in collection if c.matches_name(item.name, name_filter)]
    items.sort(key=lambda item: item.name)
    return items


def _find_by_name(kind: str, name: str):
    return {
        "objects": bpy.data.objects,
        "materials": bpy.data.materials,
        "collections": bpy.data.collections,
        "meshes": bpy.data.meshes,
        "actions": bpy.data.actions,
        "images": bpy.data.images,
    }[kind].get(name)


@op("scene.apply_transforms")
def apply_transforms(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects", required=False)
    if not objects:
        objects = [
            obj
            for obj in bpy.context.scene.objects
            if obj.type in {"MESH", "CURVE", "SURFACE", "FONT", "ARMATURE", "LATTICE"}
        ]
    location = bool(c.optional_bool(args, "location", False))
    rotation = bool(c.optional_bool(args, "rotation", False))
    scale = bool(c.optional_bool(args, "scale", True))

    if not (location or rotation or scale):
        raise invalid_argument("Enable at least one of location, rotation or scale.")

    applied, skipped = [], []
    for obj in objects:
        if obj.data is not None and getattr(obj.data, "users", 1) > 1:
            skipped.append({"object": obj.name, "reason": "data is shared with other objects"})
            continue
        if obj.library is not None:
            skipped.append({"object": obj.name, "reason": "linked from a library"})
            continue
        with c.object_mode(obj):
            try:
                bpy.ops.object.transform_apply(
                    location=location, rotation=rotation, scale=scale
                )
            except RuntimeError as error:
                skipped.append({"object": obj.name, "reason": str(error)})
                continue
        if obj.type == "MESH":
            ids.next_mesh_revision(obj.data)
        applied.append(obj.name)

    ctx.bump()
    return {
        "applied": applied,
        "skipped": skipped,
        "components": {"location": location, "rotation": rotation, "scale": scale},
        "revision": ctx.revision,
    }


@read("scene.mesh_analysis")
def mesh_analysis(ctx, args: dict) -> dict[str, Any]:
    """Run the mesh analysis over many objects at once."""
    from .mesh import analyze

    objects = c.objects_arg(args, "objects", required=False)
    if not objects:
        objects = [obj for obj in bpy.context.scene.objects if obj.type == "MESH"]

    results = []
    for obj in objects:
        if obj.type != "MESH":
            continue
        results.append(analyze(ctx, {"object": ids.ensure_id(obj)}))

    problems = [
        {
            "object": entry["object"],
            "issues": [
                name
                for name, value in (
                    ("non_manifold_edges", entry["non_manifold_edges"]),
                    ("degenerate_faces", entry["degenerate_faces"]),
                    ("loose_vertices", entry["loose_vertices"]),
                    ("loose_edges", entry["loose_edges"]),
                )
                if value
            ]
            + ([] if entry["uv_maps"] else ["no_uv_map"])
            + ([] if entry["has_applied_scale"] else ["unapplied_scale"]),
        }
        for entry in results
    ]

    return {
        "meshes": results,
        "problems": [entry for entry in problems if entry["issues"]],
        "total": len(results),
        "revision": ctx.revision,
    }

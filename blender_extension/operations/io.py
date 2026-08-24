"""Import and export.

Which operator implements a format changes between Blender releases, so the
mapping lives in :mod:`blender_extension.compatibility` and is resolved at call
time. Absolute paths always come from the Rust server, which is what keeps them
inside a managed root.
"""

from __future__ import annotations

import os
from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import external, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

AXES = ["X", "Y", "Z", "-X", "-Y", "-Z"]


@read("io.capabilities")
def capabilities(ctx, args: dict) -> dict[str, Any]:
    """Which formats this build can actually move geometry through."""
    def describe(direction: str) -> list[dict[str, Any]]:
        entries = []
        for fmt in sorted(compatibility.IO_OPERATORS):
            try:
                operator = compatibility.resolve_io_operator(fmt, direction)
            except BridgeError:
                continue
            entries.append({"format": fmt, "operator": operator})
        return entries

    return {
        "import": describe("import"),
        "export": describe("export"),
        "revision": ctx.revision,
    }


def _call_operator(path: str, kwargs: dict[str, Any], what: str) -> None:
    """Call an importer or exporter, filtering out arguments it does not have.

    Operator signatures drift between releases; passing an argument a build
    does not know raises `TypeError` and aborts the whole operation. Filtering
    against the operator's own RNA means a missing option degrades to a default
    rather than to a failure.
    """
    module, _, name = path.partition(".")
    operator = getattr(getattr(bpy.ops, module), name)

    accepted = set()
    try:
        rna = operator.get_rna_type()
        accepted = {prop.identifier for prop in rna.properties}
    except (AttributeError, RuntimeError):
        accepted = set(kwargs)

    filtered = {key: value for key, value in kwargs.items() if key in accepted}
    dropped = sorted(set(kwargs) - set(filtered))

    try:
        result = operator(**filtered)
    except (RuntimeError, TypeError) as error:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            f"{what} failed: {error}",
            {"operator": path, "arguments": sorted(filtered), "ignored": dropped},
        ) from error

    if "CANCELLED" in result:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            f"{what} was cancelled by Blender. The file may be unreadable or unsupported.",
            {"operator": path},
        )


@external("io.import")
def do_import(ctx, args: dict) -> dict[str, Any]:
    source = c.require_str(args, "source_path")
    fmt = c.require_str(args, "format").upper()

    if not os.path.isabs(source):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`source_path` must be absolute; the server supplies it.",
            {"source_path": source},
        )
    if not os.path.exists(source):
        raise BridgeError(
            ErrorCode.INVALID_PATH, f"No file at `{source}`.", {"source_path": source}
        )

    operator_path = compatibility.resolve_io_operator(fmt, "import")

    before = {obj.name for obj in bpy.data.objects}
    kwargs: dict[str, Any] = {"filepath": source}

    scale = c.optional_float(args, "scale")
    if scale is not None:
        kwargs["global_scale"] = scale
        kwargs["scale"] = scale

    forward = c.optional_str(args, "forward_axis")
    up = c.optional_str(args, "up_axis")
    if forward is not None:
        c.enum_value(forward, AXES, "forward_axis")
        # The two importer generations name these differently.
        kwargs["axis_forward"] = forward
        kwargs["forward_axis"] = forward.replace("-", "NEGATIVE_")
    if up is not None:
        c.enum_value(up, AXES, "up_axis")
        kwargs["axis_up"] = up
        kwargs["up_axis"] = up.replace("-", "NEGATIVE_")

    animation = c.optional_bool(args, "import_animation")
    if animation is not None:
        kwargs["use_anim"] = animation
        kwargs["import_anim"] = animation

    materials = c.optional_bool(args, "import_materials")
    if materials is not None:
        kwargs["use_image_search"] = materials
        kwargs["import_materials"] = materials

    _call_operator(operator_path, kwargs, f"Importing {fmt}")

    created = [obj for obj in bpy.data.objects if obj.name not in before]

    prefix = c.optional_str(args, "name_prefix")
    target_collection = c.collection_arg(args, "collection")
    for obj in created:
        if prefix:
            obj.name = f"{prefix}{obj.name}"
        if target_collection is not None:
            for existing in list(obj.users_collection):
                existing.objects.unlink(obj)
            target_collection.objects.link(obj)
        if obj.type == "MESH":
            ids.next_mesh_revision(obj.data)

    ids.invalidate_cache()
    ctx.bump()

    from .object import summarise as summarise_object

    return {
        "format": fmt,
        "operator": operator_path,
        "imported": [summarise_object(obj) for obj in created],
        "count": len(created),
        "revision": ctx.revision,
    }


@external("io.export")
def do_export(ctx, args: dict) -> dict[str, Any]:
    destination = c.require_str(args, "destination_path")
    fmt = c.require_str(args, "format").upper()

    if not os.path.isabs(destination):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`destination_path` must be absolute; the server supplies it.",
            {"destination_path": destination},
        )

    operator_path = compatibility.resolve_io_operator(fmt, "export")
    selection = c.optional(args, "selection") or {"scene": {}}
    objects, selection_kind = _resolve_selection(selection)

    kwargs: dict[str, Any] = {"filepath": destination}

    if selection_kind != "scene":
        kwargs["use_selection"] = True
        kwargs["export_selected_objects"] = True

    scale = c.optional_float(args, "scale")
    if scale is not None:
        kwargs["global_scale"] = scale
        kwargs["scale"] = scale

    forward = c.optional_str(args, "forward_axis")
    up = c.optional_str(args, "up_axis")
    if forward is not None:
        c.enum_value(forward, AXES, "forward_axis")
        kwargs["axis_forward"] = forward
        kwargs["forward_axis"] = forward.replace("-", "NEGATIVE_")
    if up is not None:
        c.enum_value(up, AXES, "up_axis")
        kwargs["axis_up"] = up
        kwargs["up_axis"] = up.replace("-", "NEGATIVE_")

    apply_modifiers = c.optional_bool(args, "apply_modifiers")
    if apply_modifiers is not None:
        kwargs["use_mesh_modifiers"] = apply_modifiers
        kwargs["apply_modifiers"] = apply_modifiers
        kwargs["export_apply"] = apply_modifiers

    triangulate = c.optional_bool(args, "triangulate")
    if triangulate is not None:
        kwargs["use_triangles"] = triangulate
        kwargs["export_triangulated_mesh"] = triangulate

    export_materials = c.optional_bool(args, "export_materials")
    if export_materials is not None:
        kwargs["export_materials"] = "EXPORT" if export_materials else "NONE"
        kwargs["use_materials"] = export_materials

    export_animation = c.optional_bool(args, "export_animation")
    if export_animation is not None:
        kwargs["use_anim"] = export_animation
        kwargs["export_animations"] = export_animation
        kwargs["bake_anim"] = export_animation

    normals = c.optional_bool(args, "export_normals")
    if normals is not None:
        kwargs["export_normals"] = normals
        kwargs["use_normals"] = normals

    uvs = c.optional_bool(args, "export_uvs")
    if uvs is not None:
        kwargs["export_uv"] = uvs
        kwargs["use_uvs"] = uvs

    if fmt == "GLB":
        kwargs["export_format"] = "GLB"
    elif fmt == "GLTF":
        kwargs["export_format"] = "GLTF_SEPARATE"

    textures = c.optional_str(args, "textures")
    if textures == "EMBED":
        kwargs["path_mode"] = "COPY"
        kwargs["embed_textures"] = True
    elif textures == "COPY":
        kwargs["path_mode"] = "COPY"
    elif textures == "STRIP":
        kwargs["path_mode"] = "STRIP"

    frame_range = c.optional(args, "frame_range")
    if frame_range is not None:
        kwargs["frame_start"] = int(frame_range[0])
        kwargs["frame_end"] = int(frame_range[1])

    # The exporters read the current selection, so it is set explicitly and put
    # back afterwards rather than assuming whatever the user left selected.
    view_layer = bpy.context.view_layer
    previous = [obj for obj in view_layer.objects if obj.select_get()]
    previous_active = view_layer.objects.active
    try:
        if selection_kind != "scene":
            for obj in view_layer.objects:
                obj.select_set(False)
            for obj in objects:
                if obj.name in view_layer.objects:
                    obj.select_set(True)
            if objects:
                view_layer.objects.active = objects[0]
        _call_operator(operator_path, kwargs, f"Exporting {fmt}")
    finally:
        for obj in view_layer.objects:
            obj.select_set(False)
        for obj in previous:
            if obj.name in view_layer.objects:
                obj.select_set(True)
        view_layer.objects.active = previous_active

    if not os.path.exists(destination):
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            "The exporter reported success but wrote no file.",
            {"destination_path": destination, "operator": operator_path},
        )

    return {
        "format": fmt,
        "operator": operator_path,
        "path": destination,
        "size_bytes": os.path.getsize(destination),
        "objects": len(objects) if selection_kind != "scene" else len(bpy.context.scene.objects),
        "revision": ctx.revision,
    }


def _resolve_selection(selection: dict) -> tuple[list, str]:
    if not isinstance(selection, dict) or len(selection) != 1:
        raise invalid_argument(
            "`selection` must be one of {\"scene\": {}}, {\"selected\": {}}, "
            "{\"objects\": [...]} or {\"collection\": \"...\"}.",
            field="selection",
        )
    kind, payload = next(iter(selection.items()))
    if kind == "scene":
        return list(bpy.context.scene.objects), "scene"
    if kind == "selected":
        selected = [obj for obj in bpy.context.view_layer.objects if obj.select_get()]
        if not selected:
            raise invalid_argument(
                "Nothing is selected, so a selection export would produce an empty file."
            )
        return selected, "selected"
    if kind == "objects":
        objects = [ids.find_object(str(reference)) for reference in payload]
        if not objects:
            raise invalid_argument("`selection.objects` is empty.")
        return objects, "objects"
    if kind == "collection":
        collection = ids.find_collection(str(payload))
        objects = _collection_objects(collection)
        if not objects:
            raise invalid_argument(
                f"Collection `{collection.name}` contains no objects to export.",
            )
        return objects, "collection"
    raise invalid_argument(f"`{kind}` is not a selection kind.", field="selection")


def _collection_objects(collection) -> list:
    found = list(collection.objects)
    for child in collection.children:
        found.extend(_collection_objects(child))
    return found


@external("file.save")
def save_file(ctx, args: dict) -> dict[str, Any]:
    """Save the .blend file.

    Only ever to a path the server resolved, and only when the caller asked --
    there is no autosave behaviour hiding in here.
    """
    path = c.optional_str(args, "destination_path")
    if path is None:
        if not bpy.data.filepath:
            raise invalid_argument(
                "This file has never been saved, so there is no path to save it to. Provide one."
            )
        path = bpy.data.filepath
    elif not os.path.isabs(path):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`destination_path` must be absolute; the server supplies it.",
            {"destination_path": path},
        )

    compress = bool(c.optional_bool(args, "compress", False))
    try:
        bpy.ops.wm.save_as_mainfile(filepath=path, compress=compress, copy=False)
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            f"Saving failed: {error}",
            {"destination_path": path},
        ) from error

    return {
        "path": path,
        "size_bytes": os.path.getsize(path) if os.path.exists(path) else 0,
        "revision": ctx.revision,
    }


@read("file.info")
def file_info(ctx, args: dict) -> dict[str, Any]:
    return {
        "filepath": bpy.data.filepath or None,
        "is_dirty": bool(bpy.data.is_dirty),
        "is_saved": bool(bpy.data.filepath),
        "blender_version": bpy.app.version_string,
        "revision": ctx.revision,
    }

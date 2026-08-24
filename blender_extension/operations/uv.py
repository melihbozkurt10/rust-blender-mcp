"""UV maps, unwrapping, seams and packing.

Unwrapping is one of the few areas where ``bpy.ops`` is genuinely required:
the unwrap algorithms are not exposed through ``bmesh``. They are therefore
driven through an explicit edit-mode context with the selection set from the
request, and the previous selection and mode restored afterwards.
"""

from __future__ import annotations

import math
from typing import Any

import bmesh
import bpy

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from .mesh import MeshEdit, selection_args

UNWRAP_METHODS = [
    "ANGLE_BASED",
    "CONFORMAL",
    "MINIMUM_STRETCH",
    "SMART_PROJECT",
    "CUBE_PROJECT",
    "CYLINDER_PROJECT",
    "SPHERE_PROJECT",
    "PROJECT_FROM_VIEW",
]

#: Which unwrap methods map onto which operator, and how their arguments are
#: named. Blender 4.3 added `MINIMUM_STRETCH` to `uv.unwrap`; older builds do
#: not have it, and the capability check reports that rather than failing
#: obscurely.
UNWRAP_OPERATORS = {
    "ANGLE_BASED": ("unwrap", {"method": "ANGLE_BASED"}),
    "CONFORMAL": ("unwrap", {"method": "CONFORMAL"}),
    "MINIMUM_STRETCH": ("unwrap", {"method": "MINIMUM_STRETCH"}),
    "SMART_PROJECT": ("smart_project", {}),
    "CUBE_PROJECT": ("cube_project", {}),
    "CYLINDER_PROJECT": ("cylinder_project", {}),
    "SPHERE_PROJECT": ("sphere_project", {}),
    "PROJECT_FROM_VIEW": ("project_from_view", {}),
}


def active_uv_layer(mesh, name: str | None):
    if name is None:
        if mesh.uv_layers.active is None:
            return mesh.uv_layers.new(name="UVMap")
        return mesh.uv_layers.active
    layer = mesh.uv_layers.get(name)
    if layer is None:
        layer = mesh.uv_layers.new(name=name)
    mesh.uv_layers.active = layer
    return layer


def _select_for_unwrap(obj, element_type: str, indices: list[int]) -> None:
    """Set the mesh selection the unwrap operators will act on."""
    mesh = obj.data
    with MeshEdit(obj, bump_revision=False) as edit:
        for face in edit.bm.faces:
            face.select = False
        for edge in edit.bm.edges:
            edge.select = False
        for vertex in edit.bm.verts:
            vertex.select = False

        if not indices:
            for face in edit.bm.faces:
                face.select = True
        else:
            elements = edit.elements(element_type, indices)
            for element in elements:
                element.select = True
            if element_type != "FACE":
                # Unwrapping works on faces; promote whatever was named.
                edit.bm.select_flush(True)
    mesh.update()


@read("uv.maps.list")
def list_maps(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    return {
        "object": ids.ensure_id(obj),
        "uv_maps": [
            {
                "name": layer.name,
                "active": layer == mesh.uv_layers.active,
                "active_render": bool(layer.active_render),
            }
            for layer in mesh.uv_layers
        ],
        "total": len(mesh.uv_layers),
        "revision": ctx.revision,
    }


@op("uv.map.create")
def create_map(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    name = c.require_str(args, "name")
    if mesh.uv_layers.get(name) is not None:
        raise invalid_argument(
            f"`{obj.name}` already has a UV map called `{name}`.", object=obj.name
        )
    if len(mesh.uv_layers) >= 8:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            "Blender allows at most 8 UV maps per mesh.",
            {"object": obj.name, "existing": [layer.name for layer in mesh.uv_layers]},
        )
    layer = mesh.uv_layers.new(name=name, do_init=bool(c.optional_bool(args, "copy_active", True)))
    mesh.uv_layers.active = layer
    ctx.bump()
    return {"object": ids.ensure_id(obj), "uv_map": layer.name, "revision": ctx.revision}


@op("uv.map.delete")
def delete_map(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    name = c.require_str(args, "name")
    layer = mesh.uv_layers.get(name)
    if layer is None:
        raise invalid_argument(
            f"`{obj.name}` has no UV map `{name}`.",
            object=obj.name,
            available=[l.name for l in mesh.uv_layers],
        )
    mesh.uv_layers.remove(layer)
    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": name, "revision": ctx.revision}


@op("uv.map.set_active")
def set_active_map(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    name = c.require_str(args, "name")
    layer = mesh.uv_layers.get(name)
    if layer is None:
        raise invalid_argument(
            f"`{obj.name}` has no UV map `{name}`.",
            available=[l.name for l in mesh.uv_layers],
        )
    mesh.uv_layers.active = layer
    if c.optional_bool(args, "active_render", False):
        layer.active_render = True
    ctx.bump()
    return {"object": ids.ensure_id(obj), "active": layer.name, "revision": ctx.revision}


def _unwrap(ctx, args: dict, method: str) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    element_type, indices, expected = selection_args(args)
    ids.check_mesh_revision(mesh, expected)

    if method == "PROJECT_FROM_VIEW" and bpy.app.background:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            "Projecting from view needs a 3D viewport, and this Blender is running headless.",
            {"method": method},
        )

    # The name is read now, not later: entering and leaving edit mode
    # reallocates the mesh's custom-data layers, which leaves any Python
    # reference to a UV layer pointing at freed memory. Reading `.name` from
    # such a reference afterwards returns whatever bytes are there now, which
    # surfaces as an intermittent UnicodeDecodeError far from the cause.
    uv_map_name = active_uv_layer(mesh, c.optional_str(args, "uv_map")).name
    _select_for_unwrap(obj, element_type, indices)

    operator_name, fixed = UNWRAP_OPERATORS[method]
    operator = getattr(bpy.ops.uv, operator_name, None)
    if operator is None:
        raise BridgeError(
            ErrorCode.CAPABILITY_UNAVAILABLE,
            f"This Blender build has no `uv.{operator_name}` operator.",
            {"method": method},
        )

    kwargs = dict(fixed)
    margin = c.optional_float(args, "margin")
    if margin is not None:
        if operator_name == "smart_project":
            # `smart_project` spells the same idea `island_margin`; passing
            # `margin` is a TypeError, not a silently ignored keyword.
            kwargs["island_margin"] = margin
        elif operator_name == "unwrap":
            kwargs["margin"] = margin
    if operator_name == "smart_project":
        angle = c.optional_float(args, "angle_limit")
        if angle is not None:
            # The protocol states degrees; the operator property is radians.
            kwargs["angle_limit"] = math.radians(angle)
        kwargs["correct_aspect"] = bool(c.optional_bool(args, "correct_aspect", True))
        kwargs["scale_to_bounds"] = bool(c.optional_bool(args, "scale_to_bounds", False))
    if operator_name == "unwrap":
        kwargs["correct_aspect"] = bool(c.optional_bool(args, "correct_aspect", True))
    if operator_name in {"cube_project", "cylinder_project", "sphere_project"}:
        size = c.optional_float(args, "projection_size")
        if size is not None and operator_name == "cube_project":
            kwargs["cube_size"] = size

    with c.object_mode(obj, "EDIT"):
        try:
            operator(**kwargs)
        except (RuntimeError, TypeError) as error:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"`uv.{operator_name}` failed: {error}",
                {"method": method, "arguments": sorted(kwargs)},
            ) from error

    ctx.bump()
    result = {
        "object": ids.ensure_id(obj),
        "method": method,
        "uv_map": uv_map_name,
        # Re-resolved from the object rather than reusing the earlier `mesh`
        # binding, for the same reason.
        "faces": len(obj.data.polygons),
        "revision": ctx.revision,
    }
    return result


@op("uv.unwrap.angle_based")
def unwrap_angle_based(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "ANGLE_BASED")


@op("uv.unwrap.conformal")
def unwrap_conformal(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "CONFORMAL")


@op("uv.smart_project")
def smart_project(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "SMART_PROJECT")


@op("uv.cube_project")
def cube_project(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "CUBE_PROJECT")


@op("uv.cylinder_project")
def cylinder_project(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "CYLINDER_PROJECT")


@op("uv.sphere_project")
def sphere_project(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "SPHERE_PROJECT")


@op("uv.project_from_view")
def project_from_view(ctx, args: dict) -> dict[str, Any]:
    return _unwrap(ctx, args, "PROJECT_FROM_VIEW")


@op("uv.pack_islands")
def pack_islands(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    margin = c.optional_float(args, "margin", 0.001) or 0.0
    rotate = c.optional_bool(args, "rotate", True)
    together = c.optional_bool(args, "pack_together", False)

    meshes = [obj for obj in objects if obj.type == "MESH"]
    if not meshes:
        raise invalid_argument("None of the given objects are meshes.")

    def pack(targets: list) -> None:
        for target in targets:
            active_uv_layer(target.data, None)
            _select_for_unwrap(target, "FACE", [])
        with c.object_mode(targets[0], "EDIT"):
            view_layer = bpy.context.view_layer
            for target in targets[1:]:
                if target.name in view_layer.objects:
                    target.select_set(True)
            try:
                bpy.ops.uv.pack_islands(margin=margin, rotate=bool(rotate))
            except (RuntimeError, TypeError) as error:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"Packing failed: {error}",
                    {"objects": [t.name for t in targets]},
                ) from error

    if together:
        pack(meshes)
    else:
        for target in meshes:
            pack([target])

    ctx.bump()
    return {
        "objects": [ids.ensure_id(target) for target in meshes],
        "packed_together": bool(together),
        "revision": ctx.revision,
    }


@op("uv.average_island_scale")
def average_island_scale(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    c.require_mesh(obj)
    active_uv_layer(obj.data, c.optional_str(args, "uv_map"))
    _select_for_unwrap(obj, "FACE", [])
    with c.object_mode(obj, "EDIT"):
        try:
            bpy.ops.uv.average_islands_scale()
        except RuntimeError as error:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"Averaging island scale failed: {error}",
                {"object": obj.name},
            ) from error
    ctx.bump()
    return {"object": ids.ensure_id(obj), "revision": ctx.revision}


def _set_seam(ctx, args: dict, value: bool) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    element_type, indices, expected = selection_args(args)
    if element_type != "EDGE":
        raise invalid_argument("Seams are marked on edges.")
    if not indices:
        raise invalid_argument(
            "Marking every edge as a seam is almost never intended; pass explicit edge indices."
        )
    ids.check_mesh_revision(mesh, expected)

    with MeshEdit(obj, bump_revision=False) as edit:
        edges = edit.elements("EDGE", indices)
        for edge in edges:
            edge.seam = value
        marked = len(edges)

    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "edges": marked,
        "seam": value,
        "revision": ctx.revision,
    }


@op("uv.mark_seam")
def mark_seam(ctx, args: dict) -> dict[str, Any]:
    return _set_seam(ctx, args, True)


@op("uv.clear_seam")
def clear_seam(ctx, args: dict) -> dict[str, Any]:
    return _set_seam(ctx, args, False)


# --- images ----------------------------------------------------------------


def summarise_image(image) -> dict[str, Any]:
    filepath = image.filepath_from_user() if image.filepath else ""
    return {
        "id": ids.ensure_id(image),
        "name": image.name,
        "filepath": image.filepath or None,
        "width": int(image.size[0]),
        "height": int(image.size[1]),
        "channels": int(image.channels),
        "is_packed": bool(image.packed_file is not None),
        "is_missing": bool(image.filepath and not image.has_data and not image.packed_file),
        "colorspace": image.colorspace_settings.name,
        "users": int(image.users),
        "source": image.source,
        "resolved_path": filepath or None,
    }


@read("image.list")
def list_images(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    missing = c.optional_bool(args, "missing")
    unused = c.optional_bool(args, "unused")

    matched = []
    for image in bpy.data.images:
        if not c.matches_name(image.name, name_filter):
            continue
        summary = summarise_image(image)
        if missing is not None and summary["is_missing"] != missing:
            continue
        if unused is not None and (image.users == 0) != unused:
            continue
        matched.append(summary)

    matched.sort(key=lambda entry: entry["name"])
    window, cursor = c.paginate(matched, args)
    return {
        "images": window,
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("image.get")
def get_image(ctx, args: dict) -> dict[str, Any]:
    image = ids.find("image", c.require_str(args, "image"))
    return {"image": summarise_image(image), "revision": ctx.revision}


@op("image.repath")
def repath_images(ctx, args: dict) -> dict[str, Any]:
    """Point images at a directory that actually holds their files.

    An asset imported from elsewhere carries the texture paths of wherever it
    came from, and Blender's "Save As" rewrites relative paths to keep them
    aimed at the old location -- so a file that moves next to its textures
    still cannot see them. Repointing by file name is what fixes that, and
    packing afterwards is what stops it happening again.

    `directory` is resolved by the server inside a managed root; the images are
    named, never paths, so nothing here chooses what may be read.
    """
    import os

    directory = c.require_str(args, "directory")
    if not os.path.isdir(directory):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            f"No directory at `{directory}`.",
            {"directory": directory},
        )

    # One lookup table for the whole call: the directory is read once, and
    # matching is case-insensitive because exporters are inconsistent about case.
    on_disk = {}
    for entry in os.listdir(directory):
        on_disk.setdefault(os.path.splitext(entry)[0].lower(), entry)

    wanted = args.get("images")
    if wanted is None:
        images = [image for image in bpy.data.images if image.source == "FILE"]
    else:
        if not isinstance(wanted, list):
            raise invalid_argument("`images` must be a list of image references.", field="images")
        images = [ids.find("image", str(reference)) for reference in wanted]

    only_missing = bool(args.get("only_missing", True))
    pack = bool(args.get("pack", False))

    repathed, packed, unmatched = [], [], []
    for image in images:
        if only_missing and not _is_missing(image):
            continue
        match = on_disk.get(os.path.splitext(image.name)[0].lower())
        if match is None:
            unmatched.append(image.name)
            continue
        image.filepath = os.path.join(directory, match)
        try:
            image.reload()
        except RuntimeError:
            unmatched.append(image.name)
            continue
        repathed.append({"name": image.name, "file": match})
        if pack:
            try:
                image.pack()
                packed.append(image.name)
            except RuntimeError:
                pass

    ctx.bump()
    return {
        "repathed": repathed,
        "packed": packed,
        "unmatched": unmatched,
        "revision": ctx.revision,
    }


def _is_missing(image) -> bool:
    """Whether Blender cannot read the file this image points at."""
    import os

    if image.packed_file is not None:
        return False
    path = bpy.path.abspath(image.filepath, library=image.library)
    return not path or not os.path.exists(path)


@op("image.load")
def load_image(ctx, args: dict) -> dict[str, Any]:
    """Load an image from a path the server resolved.

    As with rendering, the absolute path is supplied by the Rust server, which
    is what confines it to a managed root.
    """
    import os

    path = c.require_str(args, "source_path")
    if not os.path.isabs(path):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`source_path` must be absolute; the server supplies it.",
            {"source_path": path},
        )
    if not os.path.exists(path):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            f"No file at `{path}`.",
            {"source_path": path},
        )

    try:
        image = bpy.data.images.load(path, check_existing=True)
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_FORMAT,
            f"Blender could not load `{os.path.basename(path)}`: {error}",
            {"source_path": path},
        ) from error

    name = c.optional_str(args, "name")
    if name:
        image.name = name

    colorspace = c.optional_str(args, "colorspace")
    if colorspace:
        try:
            image.colorspace_settings.name = colorspace
        except TypeError as error:
            available = [
                item.identifier
                for item in bpy.types.ColorManagedInputColorspaceSettings.bl_rna.properties[
                    "name"
                ].enum_items
            ]
            raise BridgeError(
                ErrorCode.INVALID_ENUM,
                f"`{colorspace}` is not a colour space in this build.",
                {"value": colorspace, "allowed": available},
            ) from error

    if c.optional_bool(args, "pack", False):
        image.pack()

    ids.invalidate_cache("image")
    ctx.bump()
    return {"image": summarise_image(image), "revision": ctx.revision}


@op("image.reload")
def reload_image(ctx, args: dict) -> dict[str, Any]:
    image = ids.find("image", c.require_str(args, "image"))
    image.reload()
    ctx.bump()
    return {"image": summarise_image(image), "revision": ctx.revision}


@op("image.remove")
def remove_image(ctx, args: dict) -> dict[str, Any]:
    image = ids.find("image", c.require_str(args, "image"))
    payload = {"id": ids.ensure_id(image), "name": image.name}
    bpy.data.images.remove(image)
    ids.invalidate_cache("image")
    ctx.bump()
    return {"removed": payload, "revision": ctx.revision}

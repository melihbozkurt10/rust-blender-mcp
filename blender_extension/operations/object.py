"""Object operations: create, transform, hierarchy, visibility, conversion."""

from __future__ import annotations

import math
from typing import Any

import bpy
from mathutils import Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument, invalid_enum
from . import _common as c

#: Primitive -> the ``bpy.ops.mesh.primitive_*`` operator that builds it, and
#: which of its keyword arguments the protocol's options map onto.
MESH_PRIMITIVES: dict[str, tuple[str, dict[str, str]]] = {
    "CUBE": ("primitive_cube_add", {"size": "size"}),
    "PLANE": ("primitive_plane_add", {"size": "size"}),
    "UV_SPHERE": (
        "primitive_uv_sphere_add",
        {"radius": "radius", "segments": "segments", "rings": "rings"},
    ),
    "ICO_SPHERE": ("primitive_ico_sphere_add", {"radius": "radius", "subdivisions": "subdivisions"}),
    "CYLINDER": (
        "primitive_cylinder_add",
        {"radius": "radius", "depth": "depth", "vertices": "segments"},
    ),
    "CONE": (
        "primitive_cone_add",
        {"radius1": "radius", "radius2": "radius_top", "depth": "depth", "vertices": "segments"},
    ),
    "TORUS": (
        "primitive_torus_add",
        {"major_radius": "radius", "minor_radius": "minor_radius"},
    ),
    "MONKEY": ("primitive_monkey_add", {"size": "size"}),
}

OBJECT_TYPES = {
    "MESH",
    "CURVE",
    "SURFACE",
    "META",
    "FONT",
    "ARMATURE",
    "LATTICE",
    "EMPTY",
    "GPENCIL",
    "CAMERA",
    "LIGHT",
    "SPEAKER",
    "VOLUME",
}


# --- serialisation ---------------------------------------------------------


def summarise(obj, *, detail: bool = False) -> dict[str, Any]:
    """The compact object shape the server caches.

    Deliberately excludes geometry: counts yes, vertices no.
    """
    payload: dict[str, Any] = {
        "id": ids.ensure_id(obj),
        "name": obj.name,
        "type": obj.type if obj.type in OBJECT_TYPES else "OTHER",
        "location": c.vector_dict(obj.location),
        "rotation_euler": c.vector_dict(_euler_of(obj)),
        "scale": c.vector_dict(obj.scale),
        "dimensions": c.vector_dict(c.dimensions_of(obj)),
        "visible": bool(obj.visible_get()) if obj.name in bpy.context.view_layer.objects else False,
        "selected": bool(obj.select_get()) if obj.name in bpy.context.view_layer.objects else False,
    }
    if obj.parent is not None:
        payload["parent"] = ids.ensure_id(obj.parent)
    collections = [collection.name for collection in obj.users_collection]
    if collections:
        payload["collections"] = collections
    materials = [slot.material.name for slot in obj.material_slots if slot.material is not None]
    if materials:
        payload["materials"] = materials
    if obj.modifiers:
        payload["modifiers"] = [
            {
                "name": modifier.name,
                "type": modifier.type,
                "show_viewport": bool(modifier.show_viewport),
                "show_render": bool(modifier.show_render),
            }
            for modifier in obj.modifiers
        ]
    if obj.type == "MESH" and obj.data is not None:
        mesh = obj.data
        payload["mesh"] = {
            "vertices": len(mesh.vertices),
            "edges": len(mesh.edges),
            "faces": len(mesh.polygons),
            "triangles": sum(max(len(p.vertices) - 2, 0) for p in mesh.polygons),
            "revision": ids.mesh_revision(mesh),
        }
    animation = _animation_summary(obj)
    if animation is not None:
        payload["animation"] = animation
    if detail:
        payload["matrix_world"] = [list(row) for row in obj.matrix_world]
        payload["rotation_mode"] = obj.rotation_mode
        payload["hide_viewport"] = bool(obj.hide_viewport)
        payload["hide_render"] = bool(obj.hide_render)
        payload["children"] = [child.name for child in obj.children]
        if obj.constraints:
            payload["constraints"] = [
                {"name": con.name, "type": con.type} for con in obj.constraints
            ]
        payload["custom_properties"] = _custom_properties(obj)
    return payload


def _euler_of(obj) -> Vector:
    """The object's rotation as XYZ Euler, whatever mode it is in."""
    if obj.rotation_mode == "QUATERNION":
        return Vector(obj.rotation_quaternion.to_euler("XYZ"))
    if obj.rotation_mode == "AXIS_ANGLE":
        axis_angle = obj.rotation_axis_angle
        from mathutils import Quaternion

        quat = Quaternion(axis_angle[1:], axis_angle[0])
        return Vector(quat.to_euler("XYZ"))
    return Vector(obj.rotation_euler)


def _animation_summary(obj) -> dict[str, Any] | None:
    action = getattr(getattr(obj, "animation_data", None), "action", None)
    if action is None:
        return None
    curves = list(action.fcurves)
    keyframes = sum(len(curve.keyframe_points) for curve in curves)
    frame_range = list(action.frame_range) if keyframes else None
    return {
        "action": action.name,
        "fcurve_count": len(curves),
        "keyframe_count": keyframes,
        "frame_range": [float(frame_range[0]), float(frame_range[1])] if frame_range else None,
    }


def _custom_properties(datablock) -> dict[str, Any]:
    """User-set custom properties, minus the bridge's own bookkeeping."""
    from .. import config

    skip = {config.ID_PROPERTY, config.MESH_REVISION_PROPERTY, "_RNA_UI", "cycles"}
    out: dict[str, Any] = {}
    for key in datablock.keys():
        if key in skip:
            continue
        value = datablock[key]
        if isinstance(value, (int, float, str, bool)):
            out[key] = value
        elif hasattr(value, "__iter__"):
            try:
                out[key] = [float(v) for v in value]
            except (TypeError, ValueError):
                out[key] = str(value)
    return out


# --- creation --------------------------------------------------------------


@op("object.create")
def create(ctx, args: dict) -> dict[str, Any]:
    primitive = c.enum_value(
        c.require_str(args, "type"),
        [
            "EMPTY",
            "CUBE",
            "PLANE",
            "UV_SPHERE",
            "ICO_SPHERE",
            "CYLINDER",
            "CONE",
            "TORUS",
            "MONKEY",
            "CURVE",
            "TEXT",
            "CAMERA",
            "LIGHT",
        ],
        "type",
    )
    name = c.optional_str(args, "name")
    options = c.optional(args, "options", {}) or {}
    if not isinstance(options, dict):
        raise invalid_argument("`options` must be an object.", field="options")

    location = c.optional_vector(args, "location") or Vector((0.0, 0.0, 0.0))
    obj = _build(primitive, location, options)

    if name:
        obj.name = name
        if obj.data is not None and hasattr(obj.data, "name"):
            obj.data.name = name

    rotation = c.optional(args, "rotation")
    if rotation is not None:
        c.apply_rotation(obj, rotation)

    scale = c.optional_vector(args, "scale")
    if scale is not None:
        obj.scale = scale

    dimensions = c.optional_vector(args, "dimensions")
    if dimensions is not None:
        c.set_dimensions(obj, dimensions)

    target_collection = c.collection_arg(args, "collection")
    if target_collection is not None:
        _relink(obj, target_collection)

    ids.invalidate_cache("object")
    ctx.bump()
    return {"object": summarise(obj), "revision": ctx.revision}


def _build(primitive: str, location: Vector, options: dict):
    """Create the data-block and object for a primitive.

    Data APIs are used rather than operators wherever possible: operators
    depend on the current context and select what they create, which means they
    change state the caller did not ask to change.
    """
    if primitive == "EMPTY":
        obj = bpy.data.objects.new("Empty", None)
        display = options.get("empty_display_type")
        if display is not None:
            allowed = [
                item.identifier
                for item in bpy.types.Object.bl_rna.properties["empty_display_type"].enum_items
            ]
            obj.empty_display_type = c.enum_value(str(display), allowed, "options.empty_display_type")
        size = options.get("size")
        if size is not None:
            obj.empty_display_size = float(size)
        _link(obj)
    elif primitive == "CAMERA":
        data = bpy.data.cameras.new("Camera")
        obj = bpy.data.objects.new("Camera", data)
        _link(obj)
    elif primitive == "LIGHT":
        data = bpy.data.lights.new("Light", type="POINT")
        obj = bpy.data.objects.new("Light", data)
        _link(obj)
    elif primitive == "CURVE":
        data = bpy.data.curves.new("Curve", type="CURVE")
        data.dimensions = "3D"
        spline = data.splines.new("POLY")
        spline.points.add(1)
        spline.points[0].co = (0.0, 0.0, 0.0, 1.0)
        spline.points[1].co = (0.0, 1.0, 0.0, 1.0)
        obj = bpy.data.objects.new("Curve", data)
        _link(obj)
    elif primitive == "TEXT":
        data = bpy.data.curves.new("Text", type="FONT")
        data.body = str(options.get("text", "Text"))
        obj = bpy.data.objects.new("Text", data)
        _link(obj)
    else:
        obj = _build_mesh_primitive(primitive, options)

    obj.location = location
    return obj


def _build_mesh_primitive(primitive: str, options: dict):
    operator_name, mapping = MESH_PRIMITIVES[primitive]
    kwargs: dict[str, Any] = {}
    for blender_key, option_key in mapping.items():
        value = options.get(option_key)
        if value is None:
            continue
        kwargs[blender_key] = int(value) if blender_key in {"segments", "rings", "vertices", "subdivisions"} else float(value)

    operator = getattr(bpy.ops.mesh, operator_name)
    # The primitive operators are the one place operators genuinely beat the
    # data API: reimplementing Suzanne or a UV sphere by hand would be worse
    # code with no benefit. They are called with an explicit override so they
    # do not depend on whatever area the user has focused.
    with bpy.context.temp_override(**_safe_override()):
        operator(**kwargs)
    obj = bpy.context.view_layer.objects.active
    if obj is None:
        raise BridgeError(
            ErrorCode.BLENDER_CONTEXT_ERROR,
            f"Blender did not report a new object after creating a {primitive}.",
        )
    ids.next_mesh_revision(obj.data)
    return obj


def _safe_override() -> dict[str, Any]:
    """A context override that does not depend on the user's screen layout."""
    scene = bpy.context.scene
    return {
        "scene": scene,
        "view_layer": bpy.context.view_layer,
        "collection": bpy.context.view_layer.active_layer_collection.collection,
    }


def _link(obj) -> None:
    bpy.context.view_layer.active_layer_collection.collection.objects.link(obj)


def _relink(obj, collection) -> None:
    for existing in list(obj.users_collection):
        existing.objects.unlink(obj)
    collection.objects.link(obj)


# --- reading ---------------------------------------------------------------


@read("object.list")
def list_objects(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    types = set(c.optional_list(args, "types"))
    collection = c.collection_arg(args, "collection")
    selected_filter = c.optional_bool(args, "selected")
    visible_filter = c.optional_bool(args, "visible")
    material = c.optional_str(args, "material")
    has_modifier = c.optional_str(args, "has_modifier")

    allowed_objects = None
    if collection is not None:
        allowed_objects = {obj.name for obj in _collection_objects(collection)}

    matched = []
    view_layer_objects = bpy.context.view_layer.objects
    for obj in bpy.data.objects:
        if not c.matches_name(obj.name, name_filter):
            continue
        if types and obj.type not in types:
            continue
        if allowed_objects is not None and obj.name not in allowed_objects:
            continue
        in_view_layer = obj.name in view_layer_objects
        if selected_filter is not None:
            if not in_view_layer or obj.select_get() != selected_filter:
                continue
        if visible_filter is not None:
            if not in_view_layer or obj.visible_get() != visible_filter:
                continue
        if material is not None:
            names = {slot.material.name for slot in obj.material_slots if slot.material}
            resolved = ids.find_material(material, required=False)
            if resolved is None or resolved.name not in names:
                continue
        if has_modifier is not None:
            if not any(modifier.type == has_modifier for modifier in obj.modifiers):
                continue
        matched.append(obj)

    matched.sort(key=lambda o: o.name)
    window, cursor = c.paginate(matched, args)
    return {
        "objects": [summarise(obj) for obj in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


def _collection_objects(collection) -> list:
    """Every object in a collection, including nested ones."""
    found = list(collection.objects)
    for child in collection.children:
        found.extend(_collection_objects(child))
    return found


@read("object.get")
def get(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    return {"object": summarise(obj, detail=True), "revision": ctx.revision}


# --- mutation --------------------------------------------------------------


@op("object.transform")
def transform(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    relative = c.optional_bool(args, "relative", False)
    local = c.optional_bool(args, "local", False)

    location = c.optional_vector(args, "location")
    if location is not None:
        if relative:
            obj.location = Vector(obj.location) + location
        elif local and obj.parent is not None:
            obj.location = location
        else:
            _set_world_location(obj, location)

    rotation = c.optional(args, "rotation")
    if rotation is not None:
        if relative:
            _add_rotation(obj, rotation)
        else:
            c.apply_rotation(obj, rotation)

    scale = c.optional_vector(args, "scale")
    if scale is not None:
        if relative:
            obj.scale = Vector(
                (obj.scale[0] * scale[0], obj.scale[1] * scale[1], obj.scale[2] * scale[2])
            )
        else:
            obj.scale = scale

    dimensions = c.optional_vector(args, "dimensions")
    if dimensions is not None:
        c.set_dimensions(obj, dimensions)

    ctx.bump()
    return {"object": summarise(obj), "revision": ctx.revision}


def _set_world_location(obj, location: Vector) -> None:
    """Place an object at a world position, parent or not."""
    if obj.parent is None:
        obj.location = location
        return
    bpy.context.view_layer.update()
    parent_inverse = (obj.parent.matrix_world @ obj.matrix_parent_inverse).inverted_safe()
    obj.location = parent_inverse @ location


def _add_rotation(obj, rotation: dict) -> None:
    from mathutils import Euler

    kind, value = next(iter(rotation.items()))
    if kind not in {"euler", "degrees"}:
        raise invalid_argument(
            "Relative rotation takes `euler` or `degrees`; compose quaternions client-side.",
            field="rotation",
        )
    delta = c.as_vector(value, f"rotation.{kind}")
    if kind == "degrees":
        delta = Vector([math.radians(component) for component in delta])
    current = _euler_of(obj)
    obj.rotation_mode = "XYZ"
    obj.rotation_euler = Euler(Vector(current) + delta, "XYZ")


@op("object.delete")
def delete(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    delete_children = c.optional_bool(args, "delete_children", False)
    delete_data = c.optional_bool(args, "delete_data", True)

    targets = list(objects)
    if delete_children:
        for obj in objects:
            targets.extend(_descendants(obj))

    removed = []
    seen = set()
    for obj in targets:
        if obj.name in seen:
            continue
        seen.add(obj.name)
        removed.append({"id": ids.ensure_id(obj), "name": obj.name})
        data = obj.data
        bpy.data.objects.remove(obj, do_unlink=True)
        if delete_data and data is not None and data.users == 0:
            _remove_data(data)

    ids.invalidate_cache("object")
    ctx.bump()
    return {"deleted": removed, "revision": ctx.revision}


def _descendants(obj) -> list:
    found = []
    for child in obj.children:
        found.append(child)
        found.extend(_descendants(child))
    return found


def _remove_data(data) -> None:
    for collection in (
        bpy.data.meshes,
        bpy.data.curves,
        bpy.data.cameras,
        bpy.data.lights,
        bpy.data.armatures,
        bpy.data.lattices,
        bpy.data.metaballs,
    ):
        try:
            if data.name in collection and collection[data.name] == data:
                collection.remove(data)
                return
        except (TypeError, KeyError):
            continue


@op("object.duplicate")
def duplicate(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    linked = c.optional_bool(args, "linked", False)
    count = c.optional_int(args, "count", 1) or 1
    offset = c.optional_vector(args, "offset")
    name_prefix = c.optional_str(args, "name_prefix")
    collection = c.collection_arg(args, "collection")

    if count < 1 or count > 1000:
        raise invalid_argument("`count` must be between 1 and 1000.", field="count")

    created = []
    for index in range(count):
        for obj in objects:
            copy = obj.copy()
            if not linked and obj.data is not None:
                copy.data = obj.data.copy()
                if isinstance(copy.data, bpy.types.Mesh):
                    ids.next_mesh_revision(copy.data)
            # A copied data-block inherits the original's custom properties,
            # including its id. A duplicate is a new entity and must get a new
            # one, or two objects would answer to the same id.
            from .. import config

            copy.pop(config.ID_PROPERTY, None)
            if name_prefix:
                copy.name = f"{name_prefix}{obj.name}"
            if offset is not None:
                copy.location = Vector(obj.location) + offset * (index + 1)
            target = collection or (obj.users_collection[0] if obj.users_collection else None)
            if target is None:
                target = bpy.context.scene.collection
            target.objects.link(copy)
            created.append(summarise(copy))

    ids.invalidate_cache("object")
    ctx.bump()
    return {"objects": created, "revision": ctx.revision}


@op("object.rename")
def rename(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.require_str(args, "name")
    rename_data = c.optional_bool(args, "rename_data", False)
    previous = obj.name
    obj.name = name
    if rename_data and obj.data is not None and hasattr(obj.data, "name"):
        obj.data.name = name
    ids.invalidate_cache("object")
    ctx.bump()
    return {
        "id": ids.ensure_id(obj),
        "from": previous,
        "to": obj.name,
        "revision": ctx.revision,
    }


@op("object.set_parent")
def set_parent(ctx, args: dict) -> dict[str, Any]:
    child = c.object_arg(args, "object")
    parent = c.object_arg(args, "parent")
    parent_type = c.enum_value(
        c.optional_str(args, "parent_type", "OBJECT") or "OBJECT",
        ["OBJECT", "BONE", "ARMATURE", "VERTEX"],
        "parent_type",
    )
    keep_transform = c.optional_bool(args, "keep_transform", True)
    bone = c.optional_str(args, "bone")

    if child == parent:
        raise invalid_argument("An object cannot be its own parent.")
    if _is_ancestor(child, parent):
        raise invalid_argument(
            f"`{parent.name}` is already a descendant of `{child.name}`; "
            "parenting them would make a cycle.",
            object=child.name,
            parent=parent.name,
        )

    world = child.matrix_world.copy()
    child.parent = parent
    child.parent_type = parent_type
    if parent_type == "BONE":
        if bone is None:
            raise invalid_argument("`bone` is required when `parent_type` is `BONE`.", field="bone")
        if parent.type != "ARMATURE" or bone not in parent.data.bones:
            raise BridgeError(
                ErrorCode.BONE_NOT_FOUND,
                f"`{parent.name}` has no bone named `{bone}`.",
                {"armature": parent.name, "bone": bone},
            )
        child.parent_bone = bone

    if keep_transform:
        child.matrix_parent_inverse = parent.matrix_world.inverted_safe()
        child.matrix_world = world

    ctx.bump()
    return {"object": summarise(child), "revision": ctx.revision}


def _is_ancestor(candidate, obj) -> bool:
    current = obj.parent
    while current is not None:
        if current == candidate:
            return True
        current = current.parent
    return False


@op("object.clear_parent")
def clear_parent(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    keep_transform = c.optional_bool(args, "keep_transform", True)
    for obj in objects:
        world = obj.matrix_world.copy()
        obj.parent = None
        if keep_transform:
            obj.matrix_world = world
    ctx.bump()
    return {"objects": [summarise(obj) for obj in objects], "revision": ctx.revision}


@op("object.hide")
def hide(ctx, args: dict) -> dict[str, Any]:
    return _set_visibility(ctx, args, hidden=True)


@op("object.show")
def show(ctx, args: dict) -> dict[str, Any]:
    return _set_visibility(ctx, args, hidden=False)


def _set_visibility(ctx, args: dict, hidden: bool) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    viewport = c.optional_bool(args, "viewport", True)
    render = c.optional_bool(args, "render", True)
    view_layer = bpy.context.view_layer
    for obj in objects:
        if viewport:
            obj.hide_viewport = hidden
            if obj.name in view_layer.objects:
                obj.hide_set(hidden)
        if render:
            obj.hide_render = hidden
    ctx.bump()
    return {
        "objects": [{"id": ids.ensure_id(o), "name": o.name, "hidden": hidden} for o in objects],
        "revision": ctx.revision,
    }


@op("object.set_display")
def set_display(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    display_type = c.optional_str(args, "display_type")
    show_wire = c.optional_bool(args, "show_wire")
    show_in_front = c.optional_bool(args, "show_in_front")
    show_name = c.optional_bool(args, "show_name")
    color = c.optional(args, "color")

    for obj in objects:
        if display_type is not None:
            allowed = [
                item.identifier
                for item in bpy.types.Object.bl_rna.properties["display_type"].enum_items
            ]
            obj.display_type = c.enum_value(display_type, allowed, "display_type")
        if show_wire is not None:
            obj.show_wire = show_wire
        if show_in_front is not None:
            obj.show_in_front = show_in_front
        if show_name is not None:
            obj.show_name = show_name
        if color is not None:
            obj.color = c.as_color(color, "color")
    ctx.bump()
    return {"objects": [summarise(obj) for obj in objects], "revision": ctx.revision}


@op("object.transform.apply")
def apply_transform(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    location = c.optional_bool(args, "location", False)
    rotation = c.optional_bool(args, "rotation", False)
    scale = c.optional_bool(args, "scale", True)

    if not (location or rotation or scale):
        raise invalid_argument("Set at least one of location, rotation or scale.")

    applied = []
    for obj in objects:
        if obj.data is not None and obj.data.users > 1:
            # Applying a transform writes into the mesh, which every user of
            # that mesh would see. Refusing is better than silently moving
            # someone else's object.
            raise BridgeError(
                ErrorCode.UNSUPPORTED_OPERATION,
                f"`{obj.name}` shares its data with {obj.data.users - 1} other object(s); "
                "applying the transform would change them too. Make it single-user first.",
                {"object": obj.name, "users": obj.data.users},
            )
        with c.object_mode(obj):
            bpy.ops.object.transform_apply(
                location=location, rotation=rotation, scale=scale
            )
        if obj.type == "MESH":
            ids.next_mesh_revision(obj.data)
        applied.append(summarise(obj))

    ctx.bump()
    return {"objects": applied, "revision": ctx.revision}


@op("object.join")
def join(ctx, args: dict) -> dict[str, Any]:
    target = c.object_arg(args, "target")
    sources = c.objects_arg(args, "sources")
    if target in sources:
        raise invalid_argument("`target` must not appear in `sources`.")
    types = {obj.type for obj in [target, *sources]}
    if len(types) > 1:
        raise invalid_argument(
            f"Joining needs objects of one type, got {sorted(types)}.",
            types=sorted(types),
        )

    view_layer = bpy.context.view_layer
    with c.object_mode(target):
        for source in sources:
            if source.name not in view_layer.objects:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"`{source.name}` is not in the active view layer and cannot be joined.",
                    {"object": source.name},
                )
            source.select_set(True)
        bpy.ops.object.join()

    if target.type == "MESH":
        ids.next_mesh_revision(target.data)
    ids.invalidate_cache("object")
    ctx.bump()
    return {"object": summarise(target), "revision": ctx.revision}


@op("object.separate")
def separate(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    method = c.enum_value(
        c.optional_str(args, "method", "LOOSE") or "LOOSE",
        ["SELECTED", "MATERIAL", "LOOSE"],
        "method",
    )
    c.require_mesh(obj)
    before = {o.name for o in bpy.data.objects}

    with c.object_mode(obj, "EDIT"):
        bpy.ops.mesh.select_all(action="SELECT" if method == "SELECTED" else "DESELECT")
        bpy.ops.mesh.separate(type=method)

    created = [o for o in bpy.data.objects if o.name not in before]
    for new_object in created:
        from .. import config

        new_object.pop(config.ID_PROPERTY, None)
        if new_object.type == "MESH":
            ids.next_mesh_revision(new_object.data)
    ids.next_mesh_revision(obj.data)
    ids.invalidate_cache("object")
    ctx.bump()
    return {
        "object": summarise(obj),
        "created": [summarise(o) for o in created],
        "revision": ctx.revision,
    }


@op("object.origin.set")
def set_origin(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    mode = c.enum_value(
        c.require_str(args, "mode"),
        [
            "GEOMETRY_TO_ORIGIN",
            "ORIGIN_TO_GEOMETRY",
            "ORIGIN_TO_CURSOR",
            "ORIGIN_TO_CENTER_OF_MASS",
            "ORIGIN_TO_BOUNDS_CENTER",
            "ORIGIN_TO_BOUNDS_BOTTOM",
            "ORIGIN_TO_POINT",
        ],
        "mode",
    )
    point = c.optional_vector(args, "point")

    scene = bpy.context.scene
    saved_cursor = Vector(scene.cursor.location)
    try:
        for obj in objects:
            _apply_origin(obj, mode, point, scene)
    finally:
        scene.cursor.location = saved_cursor

    ctx.bump()
    return {"objects": [summarise(obj) for obj in objects], "revision": ctx.revision}


def _apply_origin(obj, mode: str, point: Vector | None, scene) -> None:
    if mode in {"ORIGIN_TO_POINT", "ORIGIN_TO_BOUNDS_CENTER", "ORIGIN_TO_BOUNDS_BOTTOM"}:
        target = _origin_target(obj, mode, point)
        scene.cursor.location = target
        with c.object_mode(obj):
            bpy.ops.object.origin_set(type="ORIGIN_CURSOR")
        return

    blender_mode = {
        "GEOMETRY_TO_ORIGIN": "GEOMETRY_ORIGIN",
        "ORIGIN_TO_GEOMETRY": "ORIGIN_GEOMETRY",
        "ORIGIN_TO_CURSOR": "ORIGIN_CURSOR",
        "ORIGIN_TO_CENTER_OF_MASS": "ORIGIN_CENTER_OF_MASS",
    }[mode]
    with c.object_mode(obj):
        bpy.ops.object.origin_set(type=blender_mode, center="MEDIAN")


def _origin_target(obj, mode: str, point: Vector | None) -> Vector:
    if mode == "ORIGIN_TO_POINT":
        if point is None:
            raise invalid_argument("`point` is required for `ORIGIN_TO_POINT`.", field="point")
        return point
    bounds = c.world_bounds([obj])
    if bounds is None:
        return Vector(obj.matrix_world.translation)
    minimum, maximum = bounds
    centre = (minimum + maximum) * 0.5
    if mode == "ORIGIN_TO_BOUNDS_BOTTOM":
        return Vector((centre.x, centre.y, minimum.z))
    return centre


@op("object.convert")
def convert(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    target = c.enum_value(
        c.require_str(args, "target"), ["MESH", "CURVE", "CURVES", "GPENCIL"], "target"
    )
    keep_original = c.optional_bool(args, "keep_original", False)

    converted = []
    for obj in objects:
        with c.object_mode(obj):
            try:
                bpy.ops.object.convert(target=target, keep_original=keep_original)
            except RuntimeError as error:
                raise BridgeError(
                    ErrorCode.UNSUPPORTED_OPERATION,
                    f"Cannot convert `{obj.name}` ({obj.type}) to {target}: {error}",
                    {"object": obj.name, "from": obj.type, "to": target},
                ) from error
        if obj.type == "MESH":
            ids.next_mesh_revision(obj.data)
        converted.append(summarise(obj))

    ids.invalidate_cache("object")
    ctx.bump()
    return {"objects": converted, "revision": ctx.revision}

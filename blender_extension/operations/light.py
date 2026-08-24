"""Light operations."""

from __future__ import annotations

import math
from typing import Any

import bpy
from mathutils import Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from .object import summarise as summarise_object

LIGHT_TYPES = ["POINT", "SUN", "SPOT", "AREA"]
AREA_SHAPES = ["SQUARE", "RECTANGLE", "DISK", "ELLIPSE"]


def kelvin_to_rgb(kelvin: float) -> list[float]:
    """Approximate blackbody colour, normalised so the brightest channel is 1.

    Done here rather than with a Blackbody node so the result is a plain colour
    that works identically in every engine and survives export.
    """
    t = max(1000.0, min(40000.0, kelvin)) / 100.0
    if t <= 66.0:
        red = 255.0
    else:
        red = 329.698727446 * ((t - 60.0) ** -0.1332047592)
    if t <= 66.0:
        green = 99.4708025861 * math.log(t) - 161.1195681661
    else:
        green = 288.1221695283 * ((t - 60.0) ** -0.0755148492)
    if t >= 66.0:
        blue = 255.0
    elif t <= 19.0:
        blue = 0.0
    else:
        blue = 138.5177312231 * math.log(t - 10.0) - 305.0447927307
    return [
        max(0.0, min(1.0, red / 255.0)),
        max(0.0, min(1.0, green / 255.0)),
        max(0.0, min(1.0, blue / 255.0)),
    ]


def summarise(obj, *, detail: bool = False) -> dict[str, Any]:
    light = obj.data
    payload: dict[str, Any] = {
        "id": ids.ensure_id(obj),
        "data_id": ids.ensure_id(light),
        "name": obj.name,
        "type": light.type,
        "location": c.vector_dict(obj.location),
        "rotation_euler": c.vector_dict(obj.rotation_euler),
        "energy": float(light.energy),
        "color": c.color_dict(light.color),
        "use_shadow": bool(getattr(light, "use_shadow", True)),
    }
    if light.type in {"POINT", "SPOT"}:
        payload["radius"] = float(light.shadow_soft_size)
    if light.type == "SUN":
        payload["angle"] = float(light.angle)
    if light.type == "SPOT":
        payload["spot_size"] = float(light.spot_size)
        payload["spot_blend"] = float(light.spot_blend)
    if light.type == "AREA":
        payload["shape"] = light.shape
        payload["size"] = float(light.size)
        payload["size_y"] = float(light.size_y)
    if detail:
        payload["diffuse_factor"] = float(light.diffuse_factor)
        payload["specular_factor"] = float(light.specular_factor)
        payload["volume_factor"] = float(light.volume_factor)
        payload["collections"] = [col.name for col in obj.users_collection]
    return payload


def apply_settings(obj, args: dict) -> list[str]:
    """Write light settings, ignoring the ones that do not apply to this type."""
    light = obj.data
    changed: list[str] = []

    energy = c.optional_float(args, "energy")
    if energy is not None:
        light.energy = energy
        changed.append("energy")

    temperature = c.optional_float(args, "temperature")
    if temperature is not None:
        light.color = kelvin_to_rgb(temperature)
        changed.append("temperature")
    elif c.optional(args, "color") is not None:
        light.color = c.as_color(args["color"], "color", length=3)
        changed.append("color")

    radius = c.optional_float(args, "radius")
    if radius is not None:
        if light.type in {"POINT", "SPOT"}:
            light.shadow_soft_size = radius
            changed.append("radius")
        else:
            _ignored(light.type, "radius")

    angle = c.optional_float(args, "angle")
    if angle is not None:
        if light.type == "SUN":
            light.angle = angle
            changed.append("angle")
        else:
            _ignored(light.type, "angle")

    shape = c.optional_str(args, "shape")
    if shape is not None:
        if light.type == "AREA":
            light.shape = c.enum_value(shape, AREA_SHAPES, "shape")
            changed.append("shape")
        else:
            _ignored(light.type, "shape")

    size = c.optional_float(args, "size")
    if size is not None:
        if light.type == "AREA":
            light.size = size
            changed.append("size")
        else:
            _ignored(light.type, "size")

    size_y = c.optional_float(args, "size_y")
    if size_y is not None:
        if light.type == "AREA":
            light.size_y = size_y
            changed.append("size_y")
        else:
            _ignored(light.type, "size_y")

    spot_size = c.optional_float(args, "spot_size")
    if spot_size is not None:
        if light.type == "SPOT":
            light.spot_size = spot_size
            changed.append("spot_size")
        else:
            _ignored(light.type, "spot_size")

    spot_blend = c.optional_float(args, "spot_blend")
    if spot_blend is not None:
        if light.type == "SPOT":
            light.spot_blend = spot_blend
            changed.append("spot_blend")
        else:
            _ignored(light.type, "spot_blend")

    use_shadow = c.optional_bool(args, "use_shadow")
    if use_shadow is not None and hasattr(light, "use_shadow"):
        light.use_shadow = use_shadow
        changed.append("use_shadow")

    for key, attribute in (
        ("diffuse_factor", "diffuse_factor"),
        ("specular_factor", "specular_factor"),
        ("volume_factor", "volume_factor"),
    ):
        value = c.optional_float(args, key)
        if value is not None and hasattr(light, attribute):
            setattr(light, attribute, value)
            changed.append(key)

    return changed


def _ignored(light_type: str, field: str) -> None:
    """A setting that does not apply to this light type is a silent no-op.

    Refusing would make it impossible to reuse one settings block across light
    types, which is exactly what a three-point rig wants to do.
    """
    print(f"[blender-mcp] `{field}` does not apply to a {light_type} light; ignored")


def aim(obj, point: Vector) -> None:
    """Point a light at a world-space location.

    A light shines down its local -Z, the same convention as a camera.
    """
    direction = point - obj.matrix_world.translation
    if direction.length < 1e-9:
        return
    obj.rotation_mode = "XYZ"
    obj.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler("XYZ")


def target_point(args: dict) -> Vector | None:
    """Where a light should aim, from `look_at` or `target`."""
    explicit = c.optional_vector(args, "look_at") or c.optional_vector(args, "point")
    if explicit is not None:
        return explicit
    reference = c.optional_str(args, "target")
    if reference is None:
        return None
    target = ids.find_object(reference)
    bounds = c.world_bounds([target])
    if bounds is None:
        return Vector(target.matrix_world.translation)
    minimum, maximum = bounds
    return (minimum + maximum) * 0.5


# --- operations ------------------------------------------------------------


@read("light.list")
def list_lights(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    type_filter = c.optional_str(args, "light_type")
    if type_filter is not None:
        type_filter = c.enum_value(type_filter, LIGHT_TYPES, "light_type")

    matched = [
        obj
        for obj in bpy.data.objects
        if obj.type == "LIGHT"
        and c.matches_name(obj.name, name_filter)
        and (type_filter is None or obj.data.type == type_filter)
    ]
    matched.sort(key=lambda o: o.name)
    window, cursor = c.paginate(matched, args)
    return {
        "lights": [summarise(obj) for obj in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("light.get")
def get(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args, "light")
    if obj.type != "LIGHT":
        raise BridgeError(
            ErrorCode.LIGHT_NOT_FOUND,
            f"`{obj.name}` is a {obj.type} object, not a light.",
            {"object": obj.name, "type": obj.type},
        )
    return {"light": summarise(obj, detail=True), "revision": ctx.revision}


@op("light.create")
def create(ctx, args: dict) -> dict[str, Any]:
    light_type = c.enum_value(c.require_str(args, "type"), LIGHT_TYPES, "type")
    name = c.optional_str(args, "name") or f"{light_type.title()} Light"

    data = bpy.data.lights.new(name, type=light_type)
    obj = bpy.data.objects.new(name, data)

    collection = c.collection_arg(args, "collection") or bpy.context.scene.collection
    collection.objects.link(obj)

    location = c.optional_vector(args, "location")
    if location is not None:
        obj.location = location

    rotation = c.optional_vector(args, "rotation")
    aim_at = target_point(args)
    if rotation is not None:
        obj.rotation_mode = "XYZ"
        obj.rotation_euler = rotation
    elif aim_at is not None:
        bpy.context.view_layer.update()
        aim(obj, aim_at)

    apply_settings(obj, args)

    ids.invalidate_cache("object")
    ctx.bump()
    return {"light": summarise(obj, detail=True), "revision": ctx.revision}


@op("light.update")
def update(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args, "light")
    if obj.type != "LIGHT":
        raise BridgeError(
            ErrorCode.LIGHT_NOT_FOUND,
            f"`{obj.name}` is a {obj.type} object, not a light.",
            {"object": obj.name},
        )
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        obj.name = name
        obj.data.name = name
        changed.append("name")
        ids.invalidate_cache("object")

    new_type = c.optional_str(args, "light_type")
    if new_type is not None:
        obj.data.type = c.enum_value(new_type, LIGHT_TYPES, "light_type")
        changed.append("type")

    changed.extend(apply_settings(obj, args))

    if not changed:
        raise invalid_argument("Nothing to update on this light.")

    ctx.bump()
    return {"light": summarise(obj, detail=True), "changed": changed, "revision": ctx.revision}


@op("light.delete")
def delete(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args, "light")
    payload = {"id": ids.ensure_id(obj), "name": obj.name}
    data = obj.data
    bpy.data.objects.remove(obj, do_unlink=True)
    if data is not None and data.users == 0:
        bpy.data.lights.remove(data)
    ids.invalidate_cache("object")
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


@op("light.look_at")
def look_at(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args, "light")
    point = target_point(args)
    if point is None:
        raise invalid_argument("Provide `point` or `target`.")

    distance = c.optional_float(args, "distance")
    if distance is not None:
        bpy.context.view_layer.update()
        direction = Vector(obj.matrix_world.translation) - point
        if direction.length < 1e-9:
            # The light sits on the target: pick a sensible default direction
            # rather than dividing by zero.
            direction = Vector((0.0, -1.0, 1.0))
        obj.location = point + direction.normalized() * distance

    bpy.context.view_layer.update()
    aim(obj, point)
    ctx.bump()
    return {"light": summarise(obj), "revision": ctx.revision}

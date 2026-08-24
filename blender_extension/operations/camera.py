"""Camera operations, including automatic framing."""

from __future__ import annotations

import math
from typing import Any

import bpy
from mathutils import Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

TRACK_CONSTRAINTS = {"TRACK_TO", "DAMPED_TRACK", "LOCKED_TRACK"}


def require_camera(obj):
    if obj.type != "CAMERA":
        raise BridgeError(
            ErrorCode.CAMERA_NOT_FOUND,
            f"`{obj.name}` is a {obj.type} object, not a camera.",
            {"object": obj.name, "type": obj.type},
        )
    return obj.data


def active_camera():
    camera = bpy.context.scene.camera
    if camera is None:
        raise BridgeError(
            ErrorCode.CAMERA_NOT_FOUND,
            "The scene has no active camera. Create one, or name a camera explicitly.",
            {"scene": bpy.context.scene.name},
        )
    return camera


def camera_arg(args: dict, key: str = "camera", required: bool = False):
    reference = c.optional_str(args, key)
    if reference is None:
        if required:
            raise invalid_argument(f"`{key}` is required.", field=key)
        return active_camera()
    obj = ids.find_object(reference)
    require_camera(obj)
    return obj


def summarise(obj, *, detail: bool = False) -> dict[str, Any]:
    camera = obj.data
    payload: dict[str, Any] = {
        "id": ids.ensure_id(obj),
        "data_id": ids.ensure_id(camera),
        "name": obj.name,
        "location": c.vector_dict(obj.location),
        "rotation_euler": c.vector_dict(obj.rotation_euler),
        "lens_mm": float(camera.lens),
        "sensor_width": float(camera.sensor_width),
        "sensor_height": float(camera.sensor_height),
        "projection": camera.type,
        "clip_start": float(camera.clip_start),
        "clip_end": float(camera.clip_end),
        "is_active": bpy.context.scene.camera == obj,
    }
    if camera.type == "ORTHO":
        payload["ortho_scale"] = float(camera.ortho_scale)
    dof = camera.dof
    payload["depth_of_field"] = {
        "enabled": bool(dof.use_dof),
        "focus_object": dof.focus_object.name if dof.focus_object else None,
        "focus_distance": float(dof.focus_distance),
        "f_stop": float(dof.aperture_fstop),
        "blades": int(dof.aperture_blades),
        "rotation": float(dof.aperture_rotation),
        "ratio": float(dof.aperture_ratio),
    }
    if obj.constraints:
        payload["constraints"] = [con.type for con in obj.constraints]
    if detail:
        payload["shift"] = {"x": float(camera.shift_x), "y": float(camera.shift_y)}
        payload["sensor_fit"] = camera.sensor_fit
        payload["fov_degrees"] = math.degrees(camera.angle)
    return payload


def apply_settings(obj, args: dict) -> list[str]:
    camera = obj.data
    changed: list[str] = []

    lens = c.optional(args, "lens")
    if lens is not None:
        if not isinstance(lens, dict) or len(lens) != 1:
            raise invalid_argument(
                "`lens` must be {\"millimetres\": n} or {\"fov_degrees\": n}.", field="lens"
            )
        kind, value = next(iter(lens.items()))
        if kind == "millimetres":
            camera.lens_unit = "MILLIMETERS"
            camera.lens = float(value)
        elif kind == "fov_degrees":
            camera.lens_unit = "FOV"
            camera.angle = math.radians(float(value))
        else:
            raise invalid_argument(
                "`lens` must be {\"millimetres\": n} or {\"fov_degrees\": n}.", field="lens"
            )
        changed.append("lens")

    projection = c.optional_str(args, "projection")
    if projection is not None:
        camera.type = {
            "PERSPECTIVE": "PERSP",
            "ORTHOGRAPHIC": "ORTHO",
            "PANORAMIC": "PANO",
        }[c.enum_value(projection, ["PERSPECTIVE", "ORTHOGRAPHIC", "PANORAMIC"], "projection")]
        changed.append("projection")

    for key, attribute in (
        ("ortho_scale", "ortho_scale"),
        ("sensor_width", "sensor_width"),
        ("sensor_height", "sensor_height"),
        ("clip_start", "clip_start"),
        ("clip_end", "clip_end"),
    ):
        value = c.optional_float(args, key)
        if value is not None:
            setattr(camera, attribute, value)
            changed.append(key)

    sensor_fit = c.optional_str(args, "sensor_fit")
    if sensor_fit is not None:
        camera.sensor_fit = c.enum_value(
            sensor_fit, ["AUTO", "HORIZONTAL", "VERTICAL"], "sensor_fit"
        )
        changed.append("sensor_fit")

    shift = c.optional(args, "shift")
    if shift is not None:
        camera.shift_x = float(shift.get("x", camera.shift_x))
        camera.shift_y = float(shift.get("y", camera.shift_y))
        changed.append("shift")

    dof_args = c.optional(args, "depth_of_field")
    if dof_args:
        changed.extend(_apply_dof(camera, dof_args))

    return changed


def _apply_dof(camera, args: dict) -> list[str]:
    dof = camera.dof
    changed: list[str] = []
    if args.get("enabled") is not None:
        dof.use_dof = bool(args["enabled"])
        changed.append("dof.enabled")
    if args.get("focus_object") is not None:
        dof.focus_object = ids.find_object(str(args["focus_object"]))
        dof.use_dof = True
        changed.append("dof.focus_object")
    if args.get("focus_distance") is not None:
        dof.focus_distance = float(args["focus_distance"])
        dof.use_dof = True
        changed.append("dof.focus_distance")
    if args.get("f_stop") is not None:
        dof.aperture_fstop = float(args["f_stop"])
        changed.append("dof.f_stop")
    if args.get("blades") is not None:
        dof.aperture_blades = int(args["blades"])
        changed.append("dof.blades")
    if args.get("rotation") is not None:
        dof.aperture_rotation = float(args["rotation"])
        changed.append("dof.rotation")
    if args.get("ratio") is not None:
        dof.aperture_ratio = float(args["ratio"])
        changed.append("dof.ratio")
    return changed


def aim(obj, point: Vector) -> None:
    """Point a camera at a world location. A camera looks down its local -Z."""
    direction = point - obj.matrix_world.translation
    if direction.length < 1e-9:
        return
    obj.rotation_mode = "XYZ"
    obj.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler("XYZ")


def subject_bounds(objects: list) -> tuple[Vector, Vector]:
    bounds = c.world_bounds(objects)
    if bounds is None:
        raise invalid_argument("The subject has no geometry to frame.")
    return bounds


# --- operations ------------------------------------------------------------


@read("camera.list")
def list_cameras(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    matched = [
        obj
        for obj in bpy.data.objects
        if obj.type == "CAMERA" and c.matches_name(obj.name, name_filter)
    ]
    matched.sort(key=lambda o: o.name)
    window, cursor = c.paginate(matched, args)
    return {
        "cameras": [summarise(obj) for obj in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("camera.get")
def get(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args)
    return {"camera": summarise(obj, detail=True), "revision": ctx.revision}


@op("camera.create")
def create(ctx, args: dict) -> dict[str, Any]:
    name = c.optional_str(args, "name") or "Camera"
    data = bpy.data.cameras.new(name)
    obj = bpy.data.objects.new(name, data)

    collection = c.collection_arg(args, "collection") or bpy.context.scene.collection
    collection.objects.link(obj)

    location = c.optional_vector(args, "location")
    if location is not None:
        obj.location = location

    apply_settings(obj, args)

    frame_targets = c.objects_arg(args, "frame_objects", required=False)
    if frame_targets:
        _frame(obj, frame_targets, args)
    else:
        rotation = c.optional_vector(args, "rotation")
        look_at = c.optional_vector(args, "look_at")
        if rotation is not None:
            obj.rotation_mode = "XYZ"
            obj.rotation_euler = rotation
        elif look_at is not None:
            bpy.context.view_layer.update()
            aim(obj, look_at)

    if c.optional_bool(args, "set_active", False):
        bpy.context.scene.camera = obj

    ids.invalidate_cache("object")
    ctx.bump()
    return {"camera": summarise(obj, detail=True), "revision": ctx.revision}


@op("camera.update")
def update(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args, required=True)
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        obj.name = name
        obj.data.name = name
        changed.append("name")
        ids.invalidate_cache("object")

    changed.extend(apply_settings(obj, args))
    if not changed:
        raise invalid_argument("Nothing to update on this camera.")

    ctx.bump()
    return {"camera": summarise(obj, detail=True), "changed": changed, "revision": ctx.revision}


@op("camera.delete")
def delete(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args, required=True)
    payload = {"id": ids.ensure_id(obj), "name": obj.name}
    data = obj.data
    bpy.data.objects.remove(obj, do_unlink=True)
    if data is not None and data.users == 0:
        bpy.data.cameras.remove(data)
    ids.invalidate_cache("object")
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


@op("camera.set_active")
def set_active(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args, required=True)
    bpy.context.scene.camera = obj
    ctx.bump()
    return {"active_camera": ids.ensure_id(obj), "name": obj.name, "revision": ctx.revision}


@op("camera.look_at")
def look_at(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args)
    point = c.optional_vector(args, "point")
    if point is None:
        target = c.optional_str(args, "target")
        if target is None:
            raise invalid_argument("Provide `point` or `target`.")
        minimum, maximum = subject_bounds([ids.find_object(target)])
        point = (minimum + maximum) * 0.5

    bpy.context.view_layer.update()
    aim(obj, point)
    ctx.bump()
    return {"camera": summarise(obj), "aimed_at": c.vector_dict(point), "revision": ctx.revision}


@op("camera.track_object")
def track_object(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args)
    target = c.object_arg(args, "target")
    constraint_type = c.enum_value(
        c.optional_str(args, "constraint", "TRACK_TO") or "TRACK_TO",
        sorted(TRACK_CONSTRAINTS),
        "constraint",
    )
    if obj == target:
        raise invalid_argument("A camera cannot track itself.")

    # Replace any existing tracking constraint rather than stacking a second
    # one, which would fight the first.
    for existing in [con for con in obj.constraints if con.type in TRACK_CONSTRAINTS]:
        obj.constraints.remove(existing)

    constraint = obj.constraints.new(type=constraint_type)
    constraint.target = target
    if constraint_type == "TRACK_TO":
        constraint.track_axis = "TRACK_NEGATIVE_Z"
        constraint.up_axis = "UP_Y"
    elif constraint_type == "DAMPED_TRACK":
        constraint.track_axis = "TRACK_NEGATIVE_Z"

    if c.optional_bool(args, "focus_on_target", False):
        obj.data.dof.use_dof = True
        obj.data.dof.focus_object = target

    ctx.bump()
    return {
        "camera": summarise(obj),
        "constraint": constraint_type,
        "target": ids.ensure_id(target),
        "revision": ctx.revision,
    }


@op("camera.clear_tracking")
def clear_tracking(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args)
    removed = []
    for existing in [con for con in obj.constraints if con.type in TRACK_CONSTRAINTS]:
        removed.append(existing.type)
        obj.constraints.remove(existing)
    ctx.bump()
    return {"camera": summarise(obj), "removed": removed, "revision": ctx.revision}


@op("camera.depth_of_field.update")
def update_dof(ctx, args: dict) -> dict[str, Any]:
    obj = camera_arg(args)
    changed = _apply_dof(obj.data, args)
    if not changed:
        raise invalid_argument("Nothing to change about the depth of field.")
    ctx.bump()
    return {"camera": summarise(obj, detail=True), "changed": changed, "revision": ctx.revision}


@op("camera.auto_frame")
def auto_frame(ctx, args: dict) -> dict[str, Any]:
    """Place and aim a camera so the subject fills the frame.

    The distance calculation lives here rather than being iterated towards:
    given the subject bounding sphere and the camera field of view, the
    distance that fits it is a closed-form expression.
    """
    obj = camera_arg(args)
    targets = c.objects_arg(args, "objects", required=False)
    if not targets:
        targets = [
            candidate
            for candidate in bpy.context.scene.objects
            if candidate.type in {"MESH", "CURVE", "FONT", "SURFACE", "META"}
            and candidate.visible_get()
        ]
    if not targets:
        raise invalid_argument("There is nothing visible to frame.")

    result = _frame(obj, targets, args)
    ctx.bump()
    return {"camera": summarise(obj), **result, "revision": ctx.revision}


def _frame(obj, targets: list, args: dict) -> dict[str, Any]:
    padding = c.optional_float(args, "padding", 0.1) or 0.0
    keep_position = c.optional_bool(args, "keep_position", False)
    should_aim = c.optional_bool(args, "aim", True)
    focus = c.optional_bool(args, "focus", False)
    direction_arg = c.optional_vector(args, "direction")

    bpy.context.view_layer.update()
    minimum, maximum = subject_bounds(targets)
    centre = (minimum + maximum) * 0.5
    radius = max((maximum - minimum).length * 0.5, 1e-6)

    camera = obj.data
    render = bpy.context.scene.render
    aspect = (render.resolution_x * render.pixel_aspect_x) / max(
        render.resolution_y * render.pixel_aspect_y, 1e-6
    )

    # Fit the bounding sphere in whichever of the two field-of-view angles is
    # tighter, so nothing is cropped in portrait or in landscape.
    horizontal_fov = camera.angle_x if hasattr(camera, "angle_x") else camera.angle
    vertical_fov = 2.0 * math.atan(math.tan(horizontal_fov * 0.5) / max(aspect, 1e-6))
    tightest = min(horizontal_fov, vertical_fov)
    distance = (radius * (1.0 + padding)) / max(math.sin(tightest * 0.5), 1e-6)

    if camera.type == "ORTHO":
        camera.ortho_scale = 2.0 * radius * (1.0 + padding)

    if not keep_position:
        if direction_arg is not None and direction_arg.length > 1e-9:
            direction = direction_arg.normalized()
        else:
            current = Vector(obj.matrix_world.translation) - centre
            if current.length > 1e-6:
                direction = current.normalized()
            else:
                # A three-quarter view from the front-left and slightly above:
                # the standard product-shot angle, and far more informative
                # than an axis-aligned one.
                direction = Vector((-0.6, -0.7, 0.38)).normalized()
        obj.location = centre + direction * distance

    bpy.context.view_layer.update()
    if should_aim:
        aim(obj, centre)

    if focus:
        camera.dof.use_dof = True
        camera.dof.focus_distance = (Vector(obj.matrix_world.translation) - centre).length

    return {
        "framed": [ids.ensure_id(target) for target in targets],
        "center": c.vector_dict(centre),
        "radius": radius,
        "distance": distance,
        "bounds": {"min": c.vector_dict(minimum), "max": c.vector_dict(maximum)},
    }

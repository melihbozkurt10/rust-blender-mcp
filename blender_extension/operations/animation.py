"""Animation: keyframes, F-curves, actions, interpolation and NLA.

Data paths are never taken from the caller as free text unless they explicitly
ask for `data_path`, and even then they are validated as RNA paths. The common
cases -- location, rotation, scale, shape keys, bone channels, material inputs
-- are named targets that this module turns into paths itself.
"""

from __future__ import annotations

import math
from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

INTERPOLATIONS = [
    "CONSTANT",
    "LINEAR",
    "BEZIER",
    "SINE",
    "QUAD",
    "CUBIC",
    "QUART",
    "QUINT",
    "EXPO",
    "CIRC",
    "BACK",
    "BOUNCE",
    "ELASTIC",
]
EASINGS = ["AUTO", "EASE_IN", "EASE_OUT", "EASE_IN_OUT"]

BONE_CHANNELS = {
    "location": "location",
    "rotation_euler": "rotation_euler",
    "rotation_quaternion": "rotation_quaternion",
    "scale": "scale",
}


def resolve_target(obj, target: dict) -> tuple[str, int | None, Any]:
    """Turn a tagged key target into an RNA path, an index and its owner."""
    if not isinstance(target, dict) or len(target) != 1:
        raise invalid_argument(
            "`target` must be a tagged object such as {\"location\": {}} or "
            "{\"bone\": {\"name\": ..., \"channel\": ...}}.",
            field="target",
        )
    kind, payload = next(iter(target.items()))

    if kind in {"location", "rotation_euler", "rotation_quaternion", "scale"}:
        return kind, None, obj
    if kind == "hide_viewport":
        return "hide_viewport", None, obj
    if kind == "hide_render":
        return "hide_render", None, obj

    if kind == "shape_key":
        name = str(payload["name"])
        mesh = obj.data
        keys = getattr(mesh, "shape_keys", None)
        if keys is None or name not in keys.key_blocks:
            raise invalid_argument(
                f"`{obj.name}` has no shape key `{name}`.",
                object=obj.name,
                available=[k.name for k in keys.key_blocks] if keys else [],
            )
        # Shape key values live on the key data-block, not on the object.
        return f'key_blocks["{name}"].value', None, keys
    if kind == "custom_property":
        name = str(payload["name"])
        if name not in obj.keys():
            raise invalid_argument(
                f"`{obj.name}` has no custom property `{name}`.", object=obj.name
            )
        return f'["{name}"]', None, obj
    if kind == "bone":
        name = str(payload["name"])
        channel = str(payload["channel"])
        if obj.type != "ARMATURE":
            raise invalid_argument(f"`{obj.name}` is not an armature.", object=obj.name)
        if name not in obj.pose.bones:
            raise BridgeError(
                ErrorCode.BONE_NOT_FOUND,
                f"`{obj.name}` has no bone `{name}`.",
                {"armature": obj.name, "available": [b.name for b in obj.pose.bones][:40]},
            )
        if channel not in BONE_CHANNELS:
            raise invalid_argument(
                f"`{channel}` is not a bone channel.",
                allowed=sorted(BONE_CHANNELS),
            )
        return f'pose.bones["{name}"].{BONE_CHANNELS[channel]}', None, obj
    if kind == "material_input":
        material = ids.find_material(str(payload["material"]))
        socket_name = str(payload["socket"])
        from .material import require_principled

        node = require_principled(material)
        socket = node.inputs.get(socket_name)
        if socket is None:
            raise BridgeError(
                ErrorCode.INVALID_NODE_SOCKET,
                f"`{socket_name}` is not an input on the Principled BSDF.",
                {"available_inputs": [s.name for s in node.inputs]},
            )
        index = list(node.inputs).index(socket)
        return f'nodes["{node.name}"].inputs[{index}].default_value', None, material.node_tree
    if kind == "data_path":
        path = str(payload["path"])
        _validate_data_path(path)
        index = payload.get("index")
        return path, int(index) if index is not None else None, obj

    raise invalid_argument(f"`{kind}` is not a keyframe target.", field="target", kind=kind)


def _validate_data_path(path: str) -> None:
    """Reject anything that is not shaped like an RNA path.

    Blender resolves data paths through its own RNA layer, not through
    `eval`, so this is defence in depth rather than the only line -- but a path
    containing parentheses or a semicolon is a mistake worth catching early
    whatever the mechanism.
    """
    if not path:
        raise invalid_argument("`path` must not be empty.", field="path")
    allowed = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.[]\"'- ")
    if not set(path) <= allowed:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"`{path}` is not a valid RNA data path.",
            {"path": path},
        )


def ensure_action(obj, create: bool = True):
    if obj.animation_data is None:
        if not create:
            return None
        obj.animation_data_create()
    if obj.animation_data.action is None:
        if not create:
            return None
        action = bpy.data.actions.new(f"{obj.name}Action")
        obj.animation_data.action = action
    return obj.animation_data.action


def _owner_for(obj, owner):
    """Make sure the data-block that carries the path has animation data."""
    if owner is obj:
        return obj
    if owner.animation_data is None:
        owner.animation_data_create()
    if owner.animation_data.action is None:
        owner.animation_data.action = bpy.data.actions.new(f"{getattr(owner, 'name', 'Data')}Action")
    return owner


def summarise_action(action) -> dict[str, Any]:
    curves = compatibility.action_fcurves(action)
    keyframes = sum(len(curve.keyframe_points) for curve in curves)
    start, end = compatibility.action_frame_range(action)
    return {
        "id": ids.ensure_id(action),
        "name": action.name,
        "frame_range": [start, end],
        "layered": compatibility.action_is_layered(action),
        "fcurve_count": len(curves),
        "keyframe_count": keyframes,
        "users": int(action.users),
        "fake_user": bool(action.use_fake_user),
    }


def summarise_fcurve(curve) -> dict[str, Any]:
    points = curve.keyframe_points
    frames = [point.co[0] for point in points]
    values = [point.co[1] for point in points]
    return {
        "data_path": curve.data_path,
        "array_index": curve.array_index,
        "keyframe_count": len(points),
        "frame_range": [min(frames), max(frames)] if frames else None,
        "value_range": [min(values), max(values)] if values else None,
        "muted": bool(curve.mute),
        "locked": bool(curve.lock),
        "modifiers": [modifier.type for modifier in curve.modifiers],
    }


# --- frames ----------------------------------------------------------------


@read("animation.frame.get")
def get_frame(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    return {
        "frame_current": scene.frame_current,
        "subframe": float(scene.frame_subframe),
        "revision": ctx.revision,
    }


@op("animation.frame.set")
def set_frame(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    frame = c.require_int(args, "frame")
    scene.frame_set(frame)
    ctx.bump()
    return {"frame_current": scene.frame_current, "revision": ctx.revision}


@read("animation.range.get")
def get_range(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    return {
        "frame_start": scene.frame_start,
        "frame_end": scene.frame_end,
        "frame_step": scene.frame_step,
        "fps": scene.render.fps / max(scene.render.fps_base, 1e-6),
        "revision": ctx.revision,
    }


@op("animation.range.set")
def set_range(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    start = c.optional_int(args, "frame_start")
    end = c.optional_int(args, "frame_end")
    step = c.optional_int(args, "frame_step")

    # Validate against the values that would result, not against the scene
    # afterwards: Blender silently drags `frame_start` down to meet a smaller
    # `frame_end`, so a post-hoc check can never fire.
    resulting_start = start if start is not None else scene.frame_start
    resulting_end = end if end is not None else scene.frame_end
    if resulting_end < resulting_start:
        raise invalid_argument(
            f"The range would end ({resulting_end}) before it starts ({resulting_start}).",
            frame_start=resulting_start,
            frame_end=resulting_end,
        )

    if start is not None:
        scene.frame_start = start
    if end is not None:
        scene.frame_end = end
    if step is not None:
        scene.frame_step = max(1, step)
    ctx.bump()
    return get_range(ctx, {})


# --- keyframes -------------------------------------------------------------


@op("animation.keyframe.insert")
def insert_keyframes(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    target = c.require(args, "target")
    keyframes = c.optional_list(args, "keyframes")
    replace = c.optional_bool(args, "replace", True)

    if not keyframes:
        raise invalid_argument("`keyframes` must not be empty.", field="keyframes")

    path, index, owner = resolve_target(obj, target)
    owner = _owner_for(obj, owner)
    ensure_action(obj) if owner is obj else None

    scene = bpy.context.scene
    saved_frame = scene.frame_current
    inserted = 0

    try:
        for keyframe in keyframes:
            frame = float(keyframe["frame"])
            value = keyframe.get("value")
            # Set the frame *before* writing the value: `frame_set` re-evaluates
            # animation and would overwrite anything written first, which is
            # exactly the silent-no-op this ordering exists to avoid.
            scene.frame_set(int(round(frame)))
            if value is not None:
                _write_value(owner, path, index, value)
            if replace:
                _remove_keyframes_at(owner, path, index, frame)
            if index is None:
                owner.keyframe_insert(data_path=path, frame=frame)
            else:
                owner.keyframe_insert(data_path=path, index=index, frame=frame)
            inserted += 1
            _apply_interpolation(owner, path, index, frame, keyframe)
    except (TypeError, RuntimeError) as error:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"Could not key `{path}`: {error}",
            {"data_path": path, "object": obj.name},
        ) from error
    finally:
        scene.frame_set(saved_frame)

    action = ensure_action(obj, create=False)
    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "data_path": path,
        "inserted": inserted,
        "action": summarise_action(action) if action else None,
        "revision": ctx.revision,
    }


def _write_value(owner, path: str, index: int | None, value: dict) -> None:
    """Set the property to a value before keying it."""
    if not isinstance(value, dict) or len(value) != 1:
        raise invalid_argument(
            "A keyframe value must be tagged, e.g. {\"vector\": {...}} or {\"scalar\": 1.0}.",
            field="value",
        )
    kind, payload = next(iter(value.items()))
    if kind == "scalar":
        decoded: Any = c.check_finite(float(payload), "value")
    elif kind == "vector":
        decoded = list(c.as_vector(payload, "value"))
    elif kind == "quaternion":
        decoded = [
            float(payload["w"]),
            float(payload["x"]),
            float(payload["y"]),
            float(payload["z"]),
        ]
    elif kind == "bool":
        decoded = bool(payload)
    else:
        raise invalid_argument(f"`{kind}` is not a keyframe value kind.", field="value")

    try:
        if path.startswith("["):
            key = path[2:-2]
            owner[key] = decoded
        elif index is not None and isinstance(decoded, (int, float, bool)):
            current = list(owner.path_resolve(path))
            current[index] = decoded
            _assign_path(owner, path, current)
        else:
            _assign_path(owner, path, decoded)
    except (AttributeError, TypeError, ValueError, KeyError) as error:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"Could not set `{path}` before keying it: {error}",
            {"data_path": path},
        ) from error


def _assign_path(owner, path: str, value: Any) -> None:
    """Assign through an RNA path without evaluating anything.

    The path is split and walked with `path_resolve`, so the final attribute is
    set through the RNA API rather than through any form of code execution.
    """
    if "." in path and not path.endswith("]"):
        head, _, tail = path.rpartition(".")
        holder = owner.path_resolve(head)
        setattr(holder, tail, value)
    elif path.endswith("]") and ".default_value" not in path:
        # e.g. nodes["X"].inputs[2].default_value handled above; a bare
        # subscript is a custom property.
        owner[path[2:-2]] = value
    else:
        setattr(owner, path, value)


def _remove_keyframes_at(owner, path: str, index: int | None, frame: float) -> None:
    for curve in compatibility.owner_fcurves(owner):
        if curve.data_path != path:
            continue
        if index is not None and curve.array_index != index:
            continue
        for point in [p for p in curve.keyframe_points if abs(p.co[0] - frame) < 1e-6]:
            curve.keyframe_points.remove(point)


def _apply_interpolation(owner, path: str, index: int | None, frame: float, keyframe: dict) -> None:
    interpolation = keyframe.get("interpolation")
    easing = keyframe.get("easing")
    if interpolation is None and easing is None:
        return
    for curve in compatibility.owner_fcurves(owner):
        if curve.data_path != path:
            continue
        if index is not None and curve.array_index != index:
            continue
        for point in curve.keyframe_points:
            if abs(point.co[0] - frame) > 1e-6:
                continue
            if interpolation is not None:
                point.interpolation = c.enum_value(
                    str(interpolation), INTERPOLATIONS, "interpolation"
                )
            if easing is not None:
                point.easing = c.enum_value(str(easing), EASINGS, "easing")


@op("animation.keyframe.delete")
def delete_keyframes(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    target = c.optional(args, "target")
    frames = [float(f) for f in c.optional_list(args, "frames")]
    frame_range = c.optional(args, "frame_range")

    if not frames and frame_range is None:
        raise invalid_argument("Provide `frames` or `frame_range`.")

    action = ensure_action(obj, create=False)
    if action is None:
        return {"object": ids.ensure_id(obj), "removed": 0, "revision": ctx.revision}

    path = None
    index = None
    if target is not None:
        path, index, _owner = resolve_target(obj, target)

    removed = 0
    for container, curve in list(compatibility.owner_fcurve_containers(obj)):
        if path is not None and curve.data_path != path:
            continue
        if index is not None and curve.array_index != index:
            continue
        for point in list(curve.keyframe_points):
            frame = point.co[0]
            hit = any(abs(frame - wanted) < 1e-6 for wanted in frames)
            if frame_range is not None:
                start, end = float(frame_range[0]), float(frame_range[1])
                hit = hit or (start <= frame <= end)
            if hit:
                curve.keyframe_points.remove(point)
                removed += 1
        if not curve.keyframe_points:
            container.remove(curve)

    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": removed, "revision": ctx.revision}


@read("animation.keyframe.list")
def list_keyframes(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    target = c.optional(args, "target")
    frame_range = c.optional(args, "frame_range")

    action = ensure_action(obj, create=False)
    if action is None:
        return {"object": ids.ensure_id(obj), "keyframes": [], "total": 0, "revision": ctx.revision}

    path = None
    index = None
    if target is not None:
        path, index, _owner = resolve_target(obj, target)

    entries = []
    for curve in compatibility.owner_fcurves(obj):
        if path is not None and curve.data_path != path:
            continue
        if index is not None and curve.array_index != index:
            continue
        for point in curve.keyframe_points:
            frame, value = float(point.co[0]), float(point.co[1])
            if frame_range is not None and not (
                float(frame_range[0]) <= frame <= float(frame_range[1])
            ):
                continue
            entries.append(
                {
                    "data_path": curve.data_path,
                    "array_index": curve.array_index,
                    "frame": frame,
                    "value": value,
                    "interpolation": point.interpolation,
                    "easing": point.easing,
                }
            )

    entries.sort(key=lambda entry: (entry["data_path"], entry["array_index"], entry["frame"]))
    window, cursor = c.paginate(entries, args)
    return {
        "object": ids.ensure_id(obj),
        "keyframes": window,
        "total": len(entries),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@op("animation.interpolation.set")
def set_interpolation(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    interpolation = c.enum_value(
        c.require_str(args, "interpolation"), INTERPOLATIONS, "interpolation"
    )
    easing = c.optional_str(args, "easing")
    target = c.optional(args, "target")
    frame_range = c.optional(args, "frame_range")

    action = ensure_action(obj, create=False)
    if action is None:
        raise invalid_argument(f"`{obj.name}` has no animation to change.", object=obj.name)

    path = index = None
    if target is not None:
        path, index, _owner = resolve_target(obj, target)

    changed = 0
    for curve in compatibility.owner_fcurves(obj):
        if path is not None and curve.data_path != path:
            continue
        if index is not None and curve.array_index != index:
            continue
        for point in curve.keyframe_points:
            frame = point.co[0]
            if frame_range is not None and not (
                float(frame_range[0]) <= frame <= float(frame_range[1])
            ):
                continue
            point.interpolation = interpolation
            if easing is not None:
                point.easing = c.enum_value(easing, EASINGS, "easing")
            changed += 1

    ctx.bump()
    return {"object": ids.ensure_id(obj), "changed": changed, "revision": ctx.revision}


# --- f-curves --------------------------------------------------------------


@read("animation.fcurve.list")
def list_fcurves(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    action = ensure_action(obj, create=False)
    curves = compatibility.owner_fcurves(obj) if action else []
    window, cursor = c.paginate(curves, args)
    return {
        "object": ids.ensure_id(obj),
        "action": action.name if action else None,
        "fcurves": [summarise_fcurve(curve) for curve in window],
        "total": len(curves),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("animation.fcurve.get")
def get_fcurve(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    data_path = c.require_str(args, "data_path")
    array_index = c.optional_int(args, "array_index", 0) or 0
    curve = _find_fcurve(obj, data_path, array_index)
    return {
        "object": ids.ensure_id(obj),
        "fcurve": summarise_fcurve(curve),
        "keyframes": [
            {
                "frame": float(point.co[0]),
                "value": float(point.co[1]),
                "interpolation": point.interpolation,
                "easing": point.easing,
            }
            for point in curve.keyframe_points
        ],
        "revision": ctx.revision,
    }


def _find_fcurve(obj, data_path: str, array_index: int):
    action = ensure_action(obj, create=False)
    if action is None:
        raise invalid_argument(f"`{obj.name}` has no action.", object=obj.name)
    for curve in compatibility.owner_fcurves(obj):
        if curve.data_path == data_path and curve.array_index == array_index:
            return curve
    raise invalid_argument(
        f"`{obj.name}` has no F-curve for `{data_path}[{array_index}]`.",
        object=obj.name,
        available=[
            {"data_path": curve.data_path, "array_index": curve.array_index}
            for curve in compatibility.owner_fcurves(obj)
        ][:40],
    )


@op("animation.fcurve.update")
def update_fcurve(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    data_path = c.require_str(args, "data_path")
    array_index = c.optional_int(args, "array_index", 0) or 0
    curve = _find_fcurve(obj, data_path, array_index)
    changed: list[str] = []

    muted = c.optional_bool(args, "muted")
    if muted is not None:
        curve.mute = muted
        changed.append("muted")

    locked = c.optional_bool(args, "locked")
    if locked is not None:
        curve.lock = locked
        changed.append("locked")

    extrapolation = c.optional_str(args, "extrapolation")
    if extrapolation is not None:
        curve.extrapolation = c.enum_value(extrapolation, ["CONSTANT", "LINEAR"], "extrapolation")
        changed.append("extrapolation")

    cyclic = c.optional_bool(args, "cyclic")
    if cyclic is not None:
        existing = next((m for m in curve.modifiers if m.type == "CYCLES"), None)
        if cyclic and existing is None:
            curve.modifiers.new(type="CYCLES")
        elif not cyclic and existing is not None:
            curve.modifiers.remove(existing)
        changed.append("cyclic")

    if not changed:
        raise invalid_argument("Nothing to update on this F-curve.")

    ctx.bump()
    return {"fcurve": summarise_fcurve(curve), "changed": changed, "revision": ctx.revision}


# --- actions ---------------------------------------------------------------


@read("animation.action.list")
def list_actions(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    matched = [a for a in bpy.data.actions if c.matches_name(a.name, name_filter)]
    matched.sort(key=lambda a: a.name)
    window, cursor = c.paginate(matched, args)
    return {
        "actions": [summarise_action(action) for action in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("animation.action.get")
def get_action(ctx, args: dict) -> dict[str, Any]:
    action = ids.find("action", c.require_str(args, "action"))
    payload = summarise_action(action)
    payload["fcurves"] = [
        summarise_fcurve(curve) for curve in compatibility.action_fcurves(action)
    ]
    return {"action": payload, "revision": ctx.revision}


@op("animation.action.create")
def create_action(ctx, args: dict) -> dict[str, Any]:
    name = c.optional_str(args, "name") or "Action"
    action = bpy.data.actions.new(name)
    action.use_fake_user = bool(c.optional_bool(args, "fake_user", False))

    reference = c.optional_str(args, "object")
    if reference is not None:
        obj = ids.find_object(reference)
        if obj.animation_data is None:
            obj.animation_data_create()
        obj.animation_data.action = action

    ids.invalidate_cache("action")
    ctx.bump()
    return {"action": summarise_action(action), "revision": ctx.revision}


@op("animation.action.assign")
def assign_action(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    reference = c.optional_str(args, "action")
    create = c.optional_bool(args, "create", False)

    if reference is None:
        if not create:
            raise invalid_argument("Provide `action`, or set `create: true`.")
        action = bpy.data.actions.new(c.optional_str(args, "name") or f"{obj.name}Action")
    else:
        action = ids.find("action", reference, required=not create)
        if action is None:
            action = bpy.data.actions.new(reference)

    if obj.animation_data is None:
        obj.animation_data_create()
    obj.animation_data.action = action
    if c.optional_bool(args, "fake_user", False):
        action.use_fake_user = True

    ids.invalidate_cache("action")
    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "action": summarise_action(action),
        "revision": ctx.revision,
    }


@op("animation.action.delete")
def delete_action(ctx, args: dict) -> dict[str, Any]:
    action = ids.find("action", c.require_str(args, "action"))
    payload = {"id": ids.ensure_id(action), "name": action.name}
    bpy.data.actions.remove(action)
    ids.invalidate_cache("action")
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


# --- generated motion ------------------------------------------------------


def _motion_args(args: dict) -> tuple[Any, int, int, str | None, bool]:
    obj = c.object_arg(args)
    start = c.require_int(args, "start_frame")
    end = c.require_int(args, "end_frame")
    if end == start:
        raise invalid_argument("Start and end frames are the same; the motion has no duration.")
    if end < start:
        raise invalid_argument(f"`end_frame` ({end}) precedes `start_frame` ({start}).")
    interpolation = c.optional_str(args, "interpolation")
    key_start = c.optional_bool(args, "key_start", True)
    return obj, start, end, interpolation, bool(key_start)


def _insert(obj, target: dict, frames: list[tuple[int, dict]], interpolation: str | None) -> int:
    keyframes = [
        {"frame": frame, "value": value, **({"interpolation": interpolation} if interpolation else {})}
        for frame, value in frames
    ]

    class _Ctx:
        revision = 0

        def bump(self):
            return 0

    result = insert_keyframes(
        _Ctx(), {"object": ids.ensure_id(obj), "target": target, "keyframes": keyframes}
    )
    return result["inserted"]


@op("animation.create_rotation")
def create_rotation(ctx, args: dict) -> dict[str, Any]:
    obj, start, end, interpolation, key_start = _motion_args(args)
    axis = c.enum_value(
        c.optional_str(args, "axis", "Z") or "Z", ["X", "Y", "Z", "NEG_X", "NEG_Y", "NEG_Z"], "axis"
    )
    degrees = c.optional_float(args, "degrees", 360.0) or 360.0
    loop_forever = c.optional_bool(args, "loop_forever", False)

    if degrees == 0.0:
        raise invalid_argument("`degrees` of 0 produces no motion.")

    sign = -1.0 if axis.startswith("NEG_") else 1.0
    component = {"X": 0, "Y": 1, "Z": 2}[axis.replace("NEG_", "")]

    obj.rotation_mode = "XYZ"
    base = list(obj.rotation_euler)
    end_rotation = list(base)
    end_rotation[component] = base[component] + math.radians(degrees) * sign

    frames = []
    if key_start:
        frames.append((start, {"vector": {"x": base[0], "y": base[1], "z": base[2]}}))
    frames.append(
        (end, {"vector": {"x": end_rotation[0], "y": end_rotation[1], "z": end_rotation[2]}})
    )

    inserted = _insert(obj, {"rotation_euler": {}}, frames, interpolation or "LINEAR")

    if loop_forever:
        for curve in compatibility.owner_fcurves(obj):
            if curve.data_path == "rotation_euler" and curve.array_index == component:
                if not any(m.type == "CYCLES" for m in curve.modifiers):
                    curve.modifiers.new(type="CYCLES")

    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "inserted": inserted,
        "axis": axis,
        "degrees": degrees,
        "frame_range": [start, end],
        "revision": ctx.revision,
    }


@op("animation.create_move")
def create_move(ctx, args: dict) -> dict[str, Any]:
    obj, start, end, interpolation, key_start = _motion_args(args)
    to = c.optional_vector(args, "to")
    by = c.optional_vector(args, "by")
    if (to is None) == (by is None):
        raise invalid_argument("Provide exactly one of `to` or `by`.")

    base = list(obj.location)
    destination = list(to) if to is not None else [base[i] + by[i] for i in range(3)]

    frames = []
    if key_start:
        frames.append((start, {"vector": {"x": base[0], "y": base[1], "z": base[2]}}))
    frames.append(
        (end, {"vector": {"x": destination[0], "y": destination[1], "z": destination[2]}})
    )
    inserted = _insert(obj, {"location": {}}, frames, interpolation)

    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "inserted": inserted,
        "frame_range": [start, end],
        "revision": ctx.revision,
    }


@op("animation.create_scale")
def create_scale(ctx, args: dict) -> dict[str, Any]:
    obj, start, end, interpolation, key_start = _motion_args(args)
    to = c.optional_vector(args, "to")
    if to is None:
        raise invalid_argument("`to` is required.", field="to")
    for axis, value in zip("xyz", to):
        if value <= 0.0:
            raise invalid_argument(f"`to.{axis}` must be greater than zero.", field=f"to.{axis}")

    base = list(obj.scale)
    frames = []
    if key_start:
        frames.append((start, {"vector": {"x": base[0], "y": base[1], "z": base[2]}}))
    frames.append((end, {"vector": {"x": to[0], "y": to[1], "z": to[2]}}))
    inserted = _insert(obj, {"scale": {}}, frames, interpolation)

    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "inserted": inserted,
        "frame_range": [start, end],
        "revision": ctx.revision,
    }


@op("animation.loop")
def loop(ctx, args: dict) -> dict[str, Any]:
    """Make an object existing animation repeat forever."""
    obj = c.object_arg(args)
    enable = c.optional_bool(args, "enabled", True)
    action = ensure_action(obj, create=False)
    if action is None:
        raise invalid_argument(f"`{obj.name}` has no animation to loop.", object=obj.name)

    changed = 0
    for curve in compatibility.owner_fcurves(obj):
        existing = next((m for m in curve.modifiers if m.type == "CYCLES"), None)
        if enable and existing is None:
            curve.modifiers.new(type="CYCLES")
            changed += 1
        elif not enable and existing is not None:
            curve.modifiers.remove(existing)
            changed += 1

    ctx.bump()
    return {"object": ids.ensure_id(obj), "curves_changed": changed, "revision": ctx.revision}


# --- NLA -------------------------------------------------------------------


@read("animation.nla.track.list")
def list_nla_tracks(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    animation_data = obj.animation_data
    tracks = list(animation_data.nla_tracks) if animation_data else []
    return {
        "object": ids.ensure_id(obj),
        "tracks": [
            {
                "name": track.name,
                "muted": bool(track.mute),
                "is_solo": bool(track.is_solo),
                "strips": [
                    {
                        "name": strip.name,
                        "action": strip.action.name if strip.action else None,
                        "frame_start": float(strip.frame_start),
                        "frame_end": float(strip.frame_end),
                        "blend_type": strip.blend_type,
                        "influence": float(strip.influence),
                        "repeat": float(strip.repeat),
                    }
                    for strip in track.strips
                ],
            }
            for track in tracks
        ],
        "total": len(tracks),
        "revision": ctx.revision,
    }


@op("animation.nla.track.create")
def create_nla_track(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.require_str(args, "name")
    if obj.animation_data is None:
        obj.animation_data_create()
    track = obj.animation_data.nla_tracks.new()
    track.name = name
    ctx.bump()
    return {"object": ids.ensure_id(obj), "track": track.name, "revision": ctx.revision}


@op("animation.nla.track.delete")
def delete_nla_track(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.require_str(args, "track")
    animation_data = obj.animation_data
    track = animation_data.nla_tracks.get(name) if animation_data else None
    if track is None:
        raise invalid_argument(
            f"`{obj.name}` has no NLA track `{name}`.",
            object=obj.name,
            available=[t.name for t in animation_data.nla_tracks] if animation_data else [],
        )
    animation_data.nla_tracks.remove(track)
    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": name, "revision": ctx.revision}


@op("animation.nla.strip.create")
def create_nla_strip(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    track_name = c.require_str(args, "track")
    action = ids.find("action", c.require_str(args, "action"))
    start = c.optional_float(args, "start_frame", 1.0) or 1.0

    if obj.animation_data is None:
        obj.animation_data_create()
    track = obj.animation_data.nla_tracks.get(track_name)
    if track is None:
        track = obj.animation_data.nla_tracks.new()
        track.name = track_name

    name = c.optional_str(args, "name") or action.name
    try:
        strip = track.strips.new(name, int(start), action)
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            f"Could not place a strip at frame {start} on `{track_name}`: {error}. "
            "Strips on one track may not overlap.",
            {"track": track_name, "start_frame": start},
        ) from error

    return _update_strip(ctx, strip, args)


@op("animation.nla.strip.update")
def update_nla_strip(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    track_name = c.require_str(args, "track")
    strip_name = c.require_str(args, "strip")
    animation_data = obj.animation_data
    track = animation_data.nla_tracks.get(track_name) if animation_data else None
    if track is None:
        raise invalid_argument(f"`{obj.name}` has no NLA track `{track_name}`.")
    strip = track.strips.get(strip_name)
    if strip is None:
        raise invalid_argument(
            f"Track `{track_name}` has no strip `{strip_name}`.",
            available=[s.name for s in track.strips],
        )
    return _update_strip(ctx, strip, args)


def _update_strip(ctx, strip, args: dict) -> dict[str, Any]:
    end = c.optional_float(args, "end_frame")
    if end is not None:
        strip.frame_end = end
    blend = c.optional_str(args, "blend_type")
    if blend is not None:
        strip.blend_type = c.enum_value(
            blend, ["REPLACE", "COMBINE", "ADD", "SUBTRACT", "MULTIPLY"], "blend_type"
        )
    influence = c.optional_float(args, "influence")
    if influence is not None:
        strip.use_animated_influence = True
        strip.influence = influence
    repeat = c.optional_float(args, "repeat")
    if repeat is not None:
        strip.repeat = repeat

    ctx.bump()
    return {
        "strip": {
            "name": strip.name,
            "action": strip.action.name if strip.action else None,
            "frame_start": float(strip.frame_start),
            "frame_end": float(strip.frame_end),
            "blend_type": strip.blend_type,
            "influence": float(strip.influence),
            "repeat": float(strip.repeat),
        },
        "revision": ctx.revision,
    }


@op("animation.nla.strip.delete")
def delete_nla_strip(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    track_name = c.require_str(args, "track")
    strip_name = c.require_str(args, "strip")
    animation_data = obj.animation_data
    track = animation_data.nla_tracks.get(track_name) if animation_data else None
    if track is None:
        raise invalid_argument(f"`{obj.name}` has no NLA track `{track_name}`.")
    strip = track.strips.get(strip_name)
    if strip is None:
        raise invalid_argument(f"Track `{track_name}` has no strip `{strip_name}`.")
    track.strips.remove(strip)
    ctx.bump()
    return {"removed": strip_name, "track": track_name, "revision": ctx.revision}

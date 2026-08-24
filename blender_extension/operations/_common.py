"""Shared helpers for operation handlers.

Argument extraction is strict on purpose. The Rust server has already validated
everything, so anything that fails here is either a protocol bug or someone
talking to the socket directly -- and in both cases a precise error beats a
Python traceback.
"""

from __future__ import annotations

import contextlib
import math
from typing import Any, Iterable, Sequence

import bpy
from mathutils import Euler, Quaternion, Vector

from .. import ids
from ..protocol import BridgeError, ErrorCode, invalid_argument, invalid_enum

# --- argument extraction ---------------------------------------------------


_MISSING = object()


def require(args: dict, key: str) -> Any:
    if key not in args or args[key] is None:
        raise invalid_argument(f"`{key}` is required.", field=key)
    return args[key]


def optional(args: dict, key: str, default: Any = None) -> Any:
    value = args.get(key, _MISSING)
    if value is _MISSING or value is None:
        return default
    return value


def require_str(args: dict, key: str) -> str:
    value = require(args, key)
    if not isinstance(value, str):
        raise invalid_argument(f"`{key}` must be a string.", field=key)
    return value


def optional_str(args: dict, key: str, default: str | None = None) -> str | None:
    value = optional(args, key)
    if value is None:
        return default
    if not isinstance(value, str):
        raise invalid_argument(f"`{key}` must be a string.", field=key)
    return value


def require_int(args: dict, key: str) -> int:
    value = require(args, key)
    if isinstance(value, bool) or not isinstance(value, int):
        raise invalid_argument(f"`{key}` must be an integer.", field=key)
    return value


def optional_int(args: dict, key: str, default: int | None = None) -> int | None:
    value = optional(args, key)
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise invalid_argument(f"`{key}` must be a number.", field=key)
    return int(value)


def optional_float(args: dict, key: str, default: float | None = None) -> float | None:
    value = optional(args, key)
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise invalid_argument(f"`{key}` must be a number.", field=key)
    return check_finite(float(value), key)


def optional_bool(args: dict, key: str, default: bool | None = None) -> bool | None:
    value = optional(args, key)
    if value is None:
        return default
    if not isinstance(value, bool):
        raise invalid_argument(f"`{key}` must be true or false.", field=key)
    return value


def optional_list(args: dict, key: str) -> list:
    value = optional(args, key)
    if value is None:
        return []
    if not isinstance(value, (list, tuple)):
        raise invalid_argument(f"`{key}` must be a list.", field=key)
    return list(value)


def check_finite(value: float, field: str) -> float:
    if math.isnan(value) or math.isinf(value):
        raise BridgeError(
            ErrorCode.INVALID_TRANSFORM,
            f"`{field}` must be a finite number.",
            {"field": field, "value": str(value)},
        )
    return value


def enum_value(value: str, allowed: Iterable[str], field: str) -> str:
    allowed = list(allowed)
    if value not in allowed:
        raise invalid_enum(field, value, allowed)
    return value


# --- geometry --------------------------------------------------------------


def as_vector(value: Any, field: str) -> Vector:
    """Accept ``{"x":..,"y":..,"z":..}`` or a three-element list."""
    if isinstance(value, dict):
        try:
            components = [float(value["x"]), float(value["y"]), float(value["z"])]
        except (KeyError, TypeError, ValueError) as exc:
            raise invalid_argument(
                f"`{field}` must have numeric x, y and z components.", field=field
            ) from exc
    elif isinstance(value, (list, tuple)) and len(value) == 3:
        try:
            components = [float(c) for c in value]
        except (TypeError, ValueError) as exc:
            raise invalid_argument(f"`{field}` must contain three numbers.", field=field) from exc
    else:
        raise invalid_argument(
            f"`{field}` must be an object with x, y and z, or a three-element list.",
            field=field,
        )
    for axis, component in zip("xyz", components):
        check_finite(component, f"{field}.{axis}")
    return Vector(components)


def optional_vector(args: dict, key: str) -> Vector | None:
    value = optional(args, key)
    return None if value is None else as_vector(value, key)


def as_color(value: Any, field: str, length: int = 4) -> list[float]:
    """Accept ``{"r":..,"g":..,"b":..,"a":..}`` or a list."""
    if isinstance(value, dict):
        components = [
            float(value.get("r", 0.0)),
            float(value.get("g", 0.0)),
            float(value.get("b", 0.0)),
            float(value.get("a", 1.0)),
        ]
    elif isinstance(value, (list, tuple)) and len(value) in (3, 4):
        components = [float(c) for c in value]
        if len(components) == 3:
            components.append(1.0)
    else:
        raise invalid_argument(f"`{field}` must be a colour object or list.", field=field)
    for name, component in zip("rgba", components):
        check_finite(component, f"{field}.{name}")
    return components[:length]


def apply_rotation(obj, rotation: dict) -> None:
    """Set an object's rotation from the protocol's tagged representation.

    Blender keeps euler and quaternion rotations in separate fields and uses
    whichever ``rotation_mode`` names, so the mode is set to match rather than
    writing a field the object is ignoring.
    """
    if not isinstance(rotation, dict) or len(rotation) != 1:
        raise invalid_argument(
            "`rotation` must be one of {\"euler\": ...}, {\"degrees\": ...} or "
            "{\"quaternion\": ...}.",
            field="rotation",
        )
    kind, value = next(iter(rotation.items()))
    if kind == "euler":
        obj.rotation_mode = "XYZ"
        obj.rotation_euler = Euler(as_vector(value, "rotation.euler"), "XYZ")
    elif kind == "degrees":
        vector = as_vector(value, "rotation.degrees")
        obj.rotation_mode = "XYZ"
        obj.rotation_euler = Euler([math.radians(c) for c in vector], "XYZ")
    elif kind == "quaternion":
        if not isinstance(value, dict):
            raise invalid_argument("`rotation.quaternion` must be an object.", field="rotation")
        try:
            quat = Quaternion(
                (
                    float(value["w"]),
                    float(value["x"]),
                    float(value["y"]),
                    float(value["z"]),
                )
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise invalid_argument(
                "`rotation.quaternion` needs numeric w, x, y and z.", field="rotation"
            ) from exc
        for name, component in zip("wxyz", quat):
            check_finite(component, f"rotation.quaternion.{name}")
        obj.rotation_mode = "QUATERNION"
        obj.rotation_quaternion = quat
    else:
        raise invalid_enum("rotation", kind, ["euler", "degrees", "quaternion"])


def local_size(obj) -> Vector:
    """The object's unscaled local bounding-box size.

    ``obj.dimensions`` is derived from the *evaluated* object, so it reads zero
    on geometry the depsgraph has not caught up with yet -- which is exactly
    the case immediately after creating a primitive. The local bound box is
    available straight away, and ``dimensions == local_size * scale`` is how
    Blender defines it anyway.
    """
    try:
        box = obj.bound_box
    except AttributeError:
        return Vector((0.0, 0.0, 0.0))
    if box is None or len(box) == 0:
        return Vector((0.0, 0.0, 0.0))
    corners = [tuple(corner) for corner in box]
    return Vector(
        tuple(
            max(corner[axis] for corner in corners) - min(corner[axis] for corner in corners)
            for axis in range(3)
        )
    )


def dimensions_of(obj) -> Vector:
    """World-space size, computed without waiting for a depsgraph evaluation."""
    size = local_size(obj)
    scale = obj.scale
    return Vector(tuple(abs(size[axis] * scale[axis]) for axis in range(3)))


def set_dimensions(obj, dimensions: Vector) -> None:
    """Scale an object so its world-space size matches ``dimensions``.

    An axis with no extent (a plane has no height) or a non-positive target is
    left alone rather than producing a division by zero or collapsing the
    object.
    """
    size = local_size(obj)
    scale = list(obj.scale)
    for axis in range(3):
        target = dimensions[axis]
        if target <= 0.0 or abs(size[axis]) < 1e-9:
            continue
        scale[axis] = target / size[axis]
    obj.scale = scale


def world_bounds(objects: Sequence[Any]) -> tuple[Vector, Vector] | None:
    """World-space axis-aligned bounds of the given objects."""
    # `matrix_world` is only recomputed when the depsgraph runs, so an object
    # moved earlier in the same request still reports its old transform and the
    # bounds come out in the wrong place. Every caller here reads a transform
    # that some other operation may just have written.
    bpy.context.view_layer.update()
    minimum = Vector((float("inf"),) * 3)
    maximum = Vector((float("-inf"),) * 3)
    found = False
    for obj in objects:
        corners = _bound_corners(obj)
        if corners is None:
            continue
        for corner in corners:
            found = True
            for axis in range(3):
                minimum[axis] = min(minimum[axis], corner[axis])
                maximum[axis] = max(maximum[axis], corner[axis])
    return (minimum, maximum) if found else None


def _bound_corners(obj) -> list[Vector] | None:
    try:
        box = obj.bound_box
    except AttributeError:
        box = None
    if box is None or len(box) == 0:
        # Empties, lights and cameras have no bounding box; their origin is the
        # only meaningful point.
        return [obj.matrix_world.translation.copy()]
    return [obj.matrix_world @ Vector(corner) for corner in box]


def vector_dict(vector) -> dict[str, float]:
    return {"x": float(vector[0]), "y": float(vector[1]), "z": float(vector[2])}


def color_dict(color, alpha: float = 1.0) -> dict[str, float]:
    values = list(color)
    while len(values) < 3:
        values.append(0.0)
    return {
        "r": float(values[0]),
        "g": float(values[1]),
        "b": float(values[2]),
        "a": float(values[3]) if len(values) > 3 else float(alpha),
    }


# --- mode handling ---------------------------------------------------------


@contextlib.contextmanager
def object_mode(obj=None, mode: str = "OBJECT"):
    """Run a block with a specific object active and in a specific mode.

    Restores the previous active object, selection and mode afterwards. Most
    operator failures in a headless bridge come from assuming the UI's current
    context, so nothing here reads what the user happens to have selected.
    """
    view_layer = bpy.context.view_layer
    previous_active = view_layer.objects.active
    previous_selection = [o for o in view_layer.objects if o.select_get()]
    previous_mode = previous_active.mode if previous_active is not None else "OBJECT"

    try:
        if obj is not None:
            if obj.name not in view_layer.objects:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"`{obj.name}` is not in the active view layer, so it cannot be edited. "
                    "Its collection may be excluded.",
                    {"object": obj.name},
                )
            if obj.hide_viewport or not obj.visible_get():
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"`{obj.name}` is hidden; Blender refuses edit-mode operators on hidden objects.",
                    {"object": obj.name, "suggested_fix": "Call object.show first."},
                )
            _deselect_all(view_layer)
            obj.select_set(True)
            view_layer.objects.active = obj
        if mode != "OBJECT":
            _set_mode(mode)
        yield
    finally:
        try:
            if bpy.context.object is not None and bpy.context.object.mode != "OBJECT":
                _set_mode("OBJECT")
            _deselect_all(view_layer)
            for previous in previous_selection:
                if previous.name in view_layer.objects:
                    previous.select_set(True)
            if previous_active is not None and previous_active.name in view_layer.objects:
                view_layer.objects.active = previous_active
                if previous_mode != "OBJECT":
                    _set_mode(previous_mode)
        except Exception as error:  # noqa: BLE001 - restoration is best effort
            print(f"[blender-mcp] could not fully restore context: {error}")


def _set_mode(mode: str) -> None:
    try:
        bpy.ops.object.mode_set(mode=mode)
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.BLENDER_MODE_ERROR,
            f"Could not switch to {mode} mode: {error}",
            {"mode": mode},
        ) from error


def _deselect_all(view_layer) -> None:
    for obj in view_layer.objects:
        if obj.select_get():
            obj.select_set(False)


def require_mesh(obj):
    """The object's mesh, or a typed error."""
    if obj.type != "MESH":
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{obj.name}` is a {obj.type} object; this operation needs a mesh.",
            {"object": obj.name, "type": obj.type},
        )
    return obj.data


def require_armature(obj):
    if obj.type != "ARMATURE":
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{obj.name}` is a {obj.type} object; this operation needs an armature.",
            {"object": obj.name, "type": obj.type},
        )
    return obj.data


# --- references ------------------------------------------------------------


def object_arg(args: dict, key: str = "object", required: bool = True):
    reference = optional_str(args, key)
    if reference is None:
        if required:
            raise invalid_argument(f"`{key}` is required.", field=key)
        return None
    return ids.find_object(reference)


def objects_arg(args: dict, key: str = "objects", required: bool = True) -> list:
    references = optional_list(args, key)
    if not references:
        if required:
            raise invalid_argument(f"`{key}` must name at least one object.", field=key)
        return []
    return [ids.find_object(reference) for reference in references]


def material_arg(args: dict, key: str = "material", required: bool = True):
    reference = optional_str(args, key)
    if reference is None:
        if required:
            raise invalid_argument(f"`{key}` is required.", field=key)
        return None
    return ids.find_material(reference)


def collection_arg(args: dict, key: str = "collection", required: bool = False):
    reference = optional_str(args, key)
    if reference is None:
        if required:
            raise invalid_argument(f"`{key}` is required.", field=key)
        return None
    return ids.find_collection(reference)


# --- pagination ------------------------------------------------------------

DEFAULT_LIMIT = 100
MAX_LIMIT = 1000


def paginate(items: list, args: dict) -> tuple[list, str | None]:
    """Apply ``limit``/``cursor`` to an ordered list.

    The cursor is the offset as a string. That is enough for a list that is
    re-derived on every call, and it fails loudly rather than silently skipping
    items if the caller passes something else.
    """
    limit = optional_int(args, "limit", DEFAULT_LIMIT) or DEFAULT_LIMIT
    limit = max(1, min(limit, MAX_LIMIT))
    cursor = optional_str(args, "cursor")
    offset = 0
    if cursor is not None:
        try:
            offset = int(cursor)
        except ValueError as exc:
            raise invalid_argument("`cursor` is not a cursor this server issued.", field="cursor") from exc
        if offset < 0:
            raise invalid_argument("`cursor` is not a cursor this server issued.", field="cursor")

    window = items[offset : offset + limit]
    next_cursor = str(offset + limit) if offset + limit < len(items) else None
    return window, next_cursor


def matches_name(name: str, needle: str | None) -> bool:
    return needle is None or needle.lower() in name.lower()

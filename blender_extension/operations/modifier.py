"""Modifier operations.

Type-specific properties go through the same checked mechanism as node
properties: the name must be a real, writable RNA property of that modifier,
and the value carries its own type. There is no path from a network string to
an unchecked ``setattr``.
"""

from __future__ import annotations

from typing import Any

import bpy

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from . import _nodes as n

#: Properties every modifier has, which are handled explicitly rather than
#: through the generic property mechanism.
COMMON_PROPERTIES = {"name", "show_viewport", "show_render", "show_in_editmode", "type"}

#: Modifiers that do nothing without a target object, and the property that
#: holds it.
TARGET_PROPERTY = {
    "BOOLEAN": "object",
    "CURVE": "object",
    "HOOK": "object",
    "LATTICE": "object",
    "SHRINKWRAP": "target",
    "ARRAY": "offset_object",
    "MIRROR": "mirror_object",
    "CAST": "object",
    "SIMPLE_DEFORM": "origin",
}


def find_modifier(obj, reference: str):
    """Resolve a modifier by stable id or by name."""
    for modifier in obj.modifiers:
        if modifier_id(modifier) == reference:
            return modifier
    modifier = obj.modifiers.get(reference)
    if modifier is not None:
        return modifier
    raise BridgeError(
        ErrorCode.MODIFIER_NOT_FOUND,
        f"`{obj.name}` has no modifier `{reference}`.",
        {
            "object": obj.name,
            "reference": reference,
            "available": [
                {"id": modifier_id(m), "name": m.name, "type": m.type} for m in obj.modifiers
            ],
        },
    )


def modifier_id(modifier) -> str:
    """A modifier stable identifier.

    Modifiers are not ID data-blocks and reject custom properties, so the
    ``mcp_id`` scheme used everywhere else does not apply. Blender 4.2 added
    ``persistent_uid``, which is exactly what is wanted: unique within the
    object and stable across renames and reordering. Older builds fall back to
    the name, which is stable enough to be useful and is documented as such.
    """
    uid = getattr(modifier, "persistent_uid", None)
    if isinstance(uid, int):
        return str(uid)
    return modifier.name


def ensure_modifier_id(modifier) -> str:
    """Kept as the name callers use; ids are intrinsic, never assigned."""
    return modifier_id(modifier)


def summarise(modifier, index: int, *, include_properties: bool = False) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "id": ensure_modifier_id(modifier),
        "name": modifier.name,
        "type": modifier.type,
        "index": index,
        "show_viewport": bool(modifier.show_viewport),
        "show_render": bool(modifier.show_render),
        "show_in_editmode": bool(modifier.show_in_editmode),
    }
    target_attribute = TARGET_PROPERTY.get(modifier.type)
    if target_attribute is not None:
        target = getattr(modifier, target_attribute, None)
        payload["target"] = target.name if target is not None else None
        payload["is_invalid"] = target is None and modifier.type in {
            "BOOLEAN",
            "CURVE",
            "HOOK",
            "LATTICE",
            "SHRINKWRAP",
        }
    if include_properties:
        payload["properties"] = _properties(modifier)
    return payload


def _properties(modifier) -> list[dict[str, Any]]:
    base = set(bpy.types.Modifier.bl_rna.properties.keys())
    out = []
    for prop in modifier.bl_rna.properties:
        if prop.identifier in base or prop.is_readonly:
            continue
        try:
            value = getattr(modifier, prop.identifier)
        except AttributeError:
            continue
        encoded = n.encode_value(value)
        if encoded is None and prop.type == "ENUM":
            encoded = {"enum": str(value)}
        if encoded is not None:
            out.append({"name": prop.identifier, "value": encoded})
    return out


def set_modifier_property(modifier, name: str, value: dict) -> None:
    if name in COMMON_PROPERTIES or name.startswith("_"):
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"`{name}` is set through its own field, not through `properties`.",
            {"property": name},
        )
    rna = modifier.bl_rna.properties.get(name)
    if rna is None:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"A {modifier.type} modifier has no property `{name}`.",
            {
                "modifier_type": modifier.type,
                "requested": name,
                "available": sorted(
                    p.identifier
                    for p in modifier.bl_rna.properties
                    if not p.is_readonly and p.identifier not in COMMON_PROPERTIES
                ),
            },
        )
    if rna.is_readonly:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"`{name}` is read-only on a {modifier.type} modifier.",
            {"modifier_type": modifier.type, "property": name},
        )

    decoded = n.decode_value(value, f"property `{name}`")
    if rna.type == "ENUM":
        allowed = [item.identifier for item in rna.enum_items]
        if decoded not in allowed:
            raise BridgeError(
                ErrorCode.INVALID_ENUM,
                f"`{decoded}` is not a valid `{name}` for a {modifier.type} modifier.",
                {"property": name, "value": decoded, "allowed": allowed},
            )

    try:
        setattr(modifier, name, decoded)
    except (TypeError, ValueError) as error:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"Could not set `{name}` on a {modifier.type} modifier: {error}",
            {"property": name, "modifier_type": modifier.type},
        ) from error


def apply_common(modifier, args: dict) -> list[str]:
    changed: list[str] = []
    for key in ("show_viewport", "show_render", "show_in_editmode"):
        value = c.optional_bool(args, key)
        if value is not None:
            setattr(modifier, key, value)
            changed.append(key)

    target_reference = c.optional_str(args, "target")
    if target_reference is not None:
        attribute = TARGET_PROPERTY.get(modifier.type)
        if attribute is None:
            raise invalid_argument(
                f"A {modifier.type} modifier has no target object.",
                modifier_type=modifier.type,
            )
        setattr(modifier, attribute, ids.find_object(target_reference))
        changed.append("target")

    return changed


# --- operations ------------------------------------------------------------


@read("modifier.list")
def list_modifiers(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    type_filter = c.optional_str(args, "modifier_type")
    include_properties = c.optional_bool(args, "include_properties", False)

    entries = [
        (index, modifier)
        for index, modifier in enumerate(obj.modifiers)
        if type_filter is None or modifier.type == type_filter
    ]
    window, cursor = c.paginate(entries, args)
    return {
        "object_id": ids.ensure_id(obj),
        "modifiers": [
            summarise(modifier, index, include_properties=bool(include_properties))
            for index, modifier in window
        ],
        "total": len(entries),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("modifier.get")
def get(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier = find_modifier(obj, c.require_str(args, "modifier"))
    index = list(obj.modifiers).index(modifier)
    return {
        "modifier": summarise(modifier, index, include_properties=True),
        "object_id": ids.ensure_id(obj),
        "revision": ctx.revision,
    }


@op("modifier.add")
def add(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier_type = c.require_str(args, "type")
    name = c.optional_str(args, "name") or modifier_type.title().replace("_", "")

    try:
        modifier = obj.modifiers.new(name=name, type=modifier_type)
    except (RuntimeError, TypeError) as error:
        raise BridgeError(
            ErrorCode.CAPABILITY_UNAVAILABLE,
            f"Cannot add a {modifier_type} modifier to `{obj.name}` ({obj.type}): {error}",
            {"object": obj.name, "object_type": obj.type, "modifier_type": modifier_type},
        ) from error

    apply_common(modifier, args)
    for assignment in c.optional_list(args, "properties"):
        set_modifier_property(modifier, str(assignment["name"]), assignment["value"])

    index = c.optional_int(args, "index")
    if index is not None:
        _move_to(obj, modifier, index)

    ensure_modifier_id(modifier)
    ctx.bump()
    position = list(obj.modifiers).index(modifier)
    return {
        "modifier": summarise(modifier, position, include_properties=True),
        "object_id": ids.ensure_id(obj),
        "revision": ctx.revision,
    }


@op("modifier.update")
def update(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier = find_modifier(obj, c.require_str(args, "modifier"))
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        modifier.name = name
        changed.append("name")

    changed.extend(apply_common(modifier, args))
    for assignment in c.optional_list(args, "properties"):
        set_modifier_property(modifier, str(assignment["name"]), assignment["value"])
        changed.append(f"property:{assignment['name']}")

    if not changed:
        raise invalid_argument("Nothing to update on this modifier.")

    ctx.bump()
    index = list(obj.modifiers).index(modifier)
    return {
        "modifier": summarise(modifier, index, include_properties=True),
        "changed": changed,
        "revision": ctx.revision,
    }


@op("modifier.move")
def move(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier = find_modifier(obj, c.require_str(args, "modifier"))
    to = c.require(args, "to")

    count = len(obj.modifiers)
    current = list(obj.modifiers).index(modifier)
    if isinstance(to, dict) and "index" in to:
        target = int(to["index"])
    elif to == "up":
        target = current - 1
    elif to == "down":
        target = current + 1
    elif to == "first":
        target = 0
    elif to == "last":
        target = count - 1
    else:
        raise invalid_argument(
            "`to` must be `up`, `down`, `first`, `last` or {\"index\": n}.", field="to"
        )

    target = max(0, min(target, count - 1))
    _move_to(obj, modifier, target)
    ctx.bump()
    return {
        "modifier": summarise(modifier, target),
        "from_index": current,
        "to_index": target,
        "revision": ctx.revision,
    }


def _move_to(obj, modifier, index: int) -> None:
    with c.object_mode(obj):
        try:
            bpy.ops.object.modifier_move_to_index(modifier=modifier.name, index=index)
        except RuntimeError as error:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"Could not move `{modifier.name}` to index {index}: {error}",
                {"modifier": modifier.name, "index": index},
            ) from error


@op("modifier.remove")
def remove(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier = find_modifier(obj, c.require_str(args, "modifier"))
    payload = {"id": ensure_modifier_id(modifier), "name": modifier.name, "type": modifier.type}
    obj.modifiers.remove(modifier)
    ctx.bump()
    return {"removed": payload, "object_id": ids.ensure_id(obj), "revision": ctx.revision}


@op("modifier.apply")
def apply(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    modifier = find_modifier(obj, c.require_str(args, "modifier"))
    apply_preceding = c.optional_bool(args, "apply_preceding", False)

    if obj.data is not None and obj.data.users > 1:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{obj.name}` shares its data with {obj.data.users - 1} other object(s); applying a "
            "modifier would change them too. Make it single-user first.",
            {"object": obj.name, "users": obj.data.users},
        )

    index = list(obj.modifiers).index(modifier)
    targets = list(obj.modifiers)[: index + 1] if apply_preceding else [modifier]
    applied = []

    with c.object_mode(obj):
        for target in targets:
            name = target.name
            try:
                bpy.ops.object.modifier_apply(modifier=name)
            except RuntimeError as error:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"Could not apply `{name}`: {error}",
                    {"object": obj.name, "modifier": name},
                ) from error
            applied.append(name)

    if obj.type == "MESH":
        ids.next_mesh_revision(obj.data)
    ctx.bump()
    return {
        "applied": applied,
        "object": ids.ensure_id(obj),
        "mesh_revision": ids.mesh_revision(obj.data) if obj.type == "MESH" else None,
        "revision": ctx.revision,
    }


@op("modifier.copy")
def copy(ctx, args: dict) -> dict[str, Any]:
    source = c.object_arg(args, "from")
    destinations = c.objects_arg(args, "to")
    wanted = c.optional_list(args, "modifiers")
    replace = c.optional_bool(args, "replace", False)

    if source in destinations:
        raise invalid_argument("The source object is also a destination.")

    to_copy = list(source.modifiers)
    if wanted:
        names = {str(name) for name in wanted}
        to_copy = [m for m in to_copy if m.name in names or modifier_id(m) in names]
        if len(to_copy) != len(names):
            found = {m.name for m in to_copy}
            raise BridgeError(
                ErrorCode.MODIFIER_NOT_FOUND,
                f"`{source.name}` does not have every named modifier.",
                {"missing": sorted(names - found), "available": [m.name for m in source.modifiers]},
            )

    results = []
    for destination in destinations:
        if replace:
            for existing in list(destination.modifiers):
                destination.modifiers.remove(existing)
        copied = []
        for modifier in to_copy:
            # A fresh modifier gets its own `persistent_uid` from Blender, so
            # there is nothing to reset here.
            new_modifier = destination.modifiers.new(name=modifier.name, type=modifier.type)
            _copy_properties(modifier, new_modifier)
            copied.append(new_modifier.name)
        results.append({"object": ids.ensure_id(destination), "modifiers": copied})

    ctx.bump()
    return {"copied": results, "revision": ctx.revision}


def _copy_properties(source, destination) -> None:
    base = set(bpy.types.Modifier.bl_rna.properties.keys()) - {"name"}
    for prop in source.bl_rna.properties:
        if prop.is_readonly or prop.identifier in base:
            continue
        try:
            setattr(destination, prop.identifier, getattr(source, prop.identifier))
        except (AttributeError, TypeError, ValueError):
            # Some properties are only writable in particular states; skipping
            # one is better than aborting the whole copy.
            continue

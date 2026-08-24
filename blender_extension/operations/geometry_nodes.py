"""Geometry node groups, interfaces, graphs and modifier attachment.

Node and link editing shares :mod:`_nodes` with the shader tools -- a node tree
is a node tree. What is specific to geometry nodes lives here: group lifecycle,
the interface API (which moved in Blender 4.0), modifier attachment, and the
declarative graph builder the Rust workflow layer targets.

The builder is why the scatter and array-along-curve workflows have no
special-case code in this file: the server works out which nodes to create,
how to wire them and what the defaults should be, then sends that plan as data.
"""

from __future__ import annotations

from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from . import _nodes as n

SOCKET_IDNAMES = {
    "GEOMETRY": "NodeSocketGeometry",
    "FLOAT": "NodeSocketFloat",
    "INT": "NodeSocketInt",
    "BOOL": "NodeSocketBool",
    "VECTOR": "NodeSocketVector",
    "COLOR": "NodeSocketColor",
    "STRING": "NodeSocketString",
    "OBJECT": "NodeSocketObject",
    "COLLECTION": "NodeSocketCollection",
    "MATERIAL": "NodeSocketMaterial",
    "IMAGE": "NodeSocketImage",
    "ROTATION": "NodeSocketRotation",
    "MENU": "NodeSocketMenu",
}


def summarise_group(group, *, detail: bool = False) -> dict[str, Any]:
    users = [
        obj.name
        for obj in bpy.data.objects
        for modifier in obj.modifiers
        if modifier.type == "NODES" and modifier.node_group == group
    ]
    payload: dict[str, Any] = {
        "id": ids.ensure_id(group),
        "name": group.name,
        "node_count": len(group.nodes),
        "users": int(group.users),
        "used_by": users,
    }
    if detail:
        inputs, outputs = _interface_sockets(group)
        payload["inputs"] = inputs
        payload["outputs"] = outputs
    return payload


def _interface_items(group):
    """Interface entries, whichever API this build has."""
    if compatibility.uses_tree_interface() and hasattr(group, "interface"):
        return list(group.interface.items_tree)
    # Pre-4.0: separate inputs and outputs collections.
    legacy = []
    for socket in getattr(group, "inputs", []):
        legacy.append(("INPUT", socket))
    for socket in getattr(group, "outputs", []):
        legacy.append(("OUTPUT", socket))
    return legacy


def _interface_sockets(group) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    inputs: list[dict[str, Any]] = []
    outputs: list[dict[str, Any]] = []

    if compatibility.uses_tree_interface() and hasattr(group, "interface"):
        for item in group.interface.items_tree:
            if getattr(item, "item_type", "SOCKET") != "SOCKET":
                continue
            entry = {
                "identifier": item.identifier,
                "name": item.name,
                "type": item.socket_type,
                "description": item.description or None,
            }
            for attribute in ("min_value", "max_value"):
                if hasattr(item, attribute):
                    entry["min" if attribute == "min_value" else "max"] = float(
                        getattr(item, attribute)
                    )
            if hasattr(item, "default_value"):
                encoded = n.encode_value(item.default_value)
                if encoded is not None:
                    entry["default_value"] = encoded
            (inputs if item.in_out == "INPUT" else outputs).append(entry)
        return inputs, outputs

    for direction, socket in _interface_items(group):
        entry = {
            "identifier": socket.identifier,
            "name": socket.name,
            "type": socket.bl_socket_idname,
        }
        (inputs if direction == "INPUT" else outputs).append(entry)
    return inputs, outputs


def group_arg(args: dict, key: str = "group"):
    return ids.find("node_tree", c.require_str(args, key))


@read("geometry_nodes.group.list")
def list_groups(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    used_by = c.optional_str(args, "used_by")

    groups = [g for g in bpy.data.node_groups if g.bl_idname == "GeometryNodeTree"]
    if used_by is not None:
        obj = ids.find_object(used_by)
        attached = {
            modifier.node_group
            for modifier in obj.modifiers
            if modifier.type == "NODES" and modifier.node_group is not None
        }
        groups = [g for g in groups if g in attached]
    groups = [g for g in groups if c.matches_name(g.name, name_filter)]
    groups.sort(key=lambda g: g.name)

    window, cursor = c.paginate(groups, args)
    return {
        "groups": [summarise_group(group) for group in window],
        "total": len(groups),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("geometry_nodes.group.get")
def get_group(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    return {"group": summarise_group(group, detail=True), "revision": ctx.revision}


@op("geometry_nodes.group.create")
def create_group(ctx, args: dict) -> dict[str, Any]:
    name = c.require_str(args, "name")
    with_io = c.optional_bool(args, "with_geometry_io", True)

    group = bpy.data.node_groups.new(name, "GeometryNodeTree")

    if with_io:
        _add_socket(group, "Geometry", "GEOMETRY", "INPUT")
        _add_socket(group, "Geometry", "GEOMETRY", "OUTPUT")
        input_node = group.nodes.new("NodeGroupInput")
        output_node = group.nodes.new("NodeGroupOutput")
        input_node.location = (-300, 0)
        output_node.location = (300, 0)
        if input_node.outputs and output_node.inputs:
            group.links.new(input_node.outputs[0], output_node.inputs[0])
        n.ensure_node_id(input_node)
        n.ensure_node_id(output_node)

    attach_to = c.optional_str(args, "attach_to")
    attached = None
    if attach_to is not None:
        obj = ids.find_object(attach_to)
        modifier = obj.modifiers.new(name=group.name, type="NODES")
        modifier.node_group = group
        attached = {"object": ids.ensure_id(obj), "modifier": modifier.name}

    ids.invalidate_cache("node_tree")
    ctx.bump()
    return {
        "group": summarise_group(group, detail=True),
        "attached": attached,
        "revision": ctx.revision,
    }


@op("geometry_nodes.group.delete")
def delete_group(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    if group.users > 0 and not c.optional_bool(args, "force", False):
        raise invalid_argument(
            f"`{group.name}` is used by {group.users} modifier(s). Detach it first, or pass "
            "force:true.",
            group=group.name,
            users=group.users,
        )
    payload = {"id": ids.ensure_id(group), "name": group.name}
    bpy.data.node_groups.remove(group)
    ids.invalidate_cache("node_tree")
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


# --- interface -------------------------------------------------------------


def _add_socket(group, name: str, socket_type: str, direction: str, **options: Any):
    idname = SOCKET_IDNAMES.get(socket_type)
    if idname is None:
        raise invalid_argument(
            f"`{socket_type}` is not an interface socket type.",
            allowed=sorted(SOCKET_IDNAMES),
        )

    if compatibility.uses_tree_interface() and hasattr(group, "interface"):
        item = group.interface.new_socket(name=name, in_out=direction, socket_type=idname)
    else:
        collection = group.inputs if direction == "INPUT" else group.outputs
        item = collection.new(idname, name)

    for attribute, key in (("min_value", "min"), ("max_value", "max")):
        value = options.get(key)
        if value is not None and hasattr(item, attribute):
            setattr(item, attribute, float(value))
    description = options.get("description")
    if description is not None and hasattr(item, "description"):
        item.description = str(description)
    default = options.get("default_value")
    if default is not None and hasattr(item, "default_value"):
        item.default_value = n.decode_value(default, f"default for `{name}`")
    return item


@read("geometry_nodes.interface.list")
def list_interface(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    inputs, outputs = _interface_sockets(group)
    return {
        "group": ids.ensure_id(group),
        "inputs": inputs,
        "outputs": outputs,
        "api": "interface" if compatibility.uses_tree_interface() else "legacy",
        "revision": ctx.revision,
    }


@op("geometry_nodes.interface.add_socket")
def add_interface_socket(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    name = c.require_str(args, "name")
    socket_type = c.enum_value(c.require_str(args, "type"), sorted(SOCKET_IDNAMES), "type")
    direction = c.enum_value(
        (c.optional_str(args, "direction", "input") or "input").upper(), ["INPUT", "OUTPUT"], "direction"
    )
    item = _add_socket(
        group,
        name,
        socket_type,
        direction,
        min=c.optional_float(args, "min"),
        max=c.optional_float(args, "max"),
        description=c.optional_str(args, "description"),
        default_value=c.optional(args, "default_value"),
    )
    ctx.bump()
    return {
        "group": ids.ensure_id(group),
        "socket": {"identifier": item.identifier, "name": item.name, "direction": direction},
        "revision": ctx.revision,
    }


@op("geometry_nodes.interface.update_socket")
def update_interface_socket(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    identifier = c.require_str(args, "socket")
    item = _find_interface_item(group, identifier)
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        item.name = name
        changed.append("name")
    for key, attribute in (("min", "min_value"), ("max", "max_value")):
        value = c.optional_float(args, key)
        if value is not None and hasattr(item, attribute):
            setattr(item, attribute, value)
            changed.append(key)
    description = c.optional_str(args, "description")
    if description is not None and hasattr(item, "description"):
        item.description = description
        changed.append("description")
    default = c.optional(args, "default_value")
    if default is not None and hasattr(item, "default_value"):
        item.default_value = n.decode_value(default, "default_value")
        changed.append("default_value")

    if not changed:
        raise invalid_argument("Nothing to update on this interface socket.")

    ctx.bump()
    return {"group": ids.ensure_id(group), "changed": changed, "revision": ctx.revision}


@op("geometry_nodes.interface.delete_socket")
def delete_interface_socket(ctx, args: dict) -> dict[str, Any]:
    group = group_arg(args)
    identifier = c.require_str(args, "socket")
    item = _find_interface_item(group, identifier)
    name = item.name

    if compatibility.uses_tree_interface() and hasattr(group, "interface"):
        group.interface.remove(item)
    else:
        for collection in (group.inputs, group.outputs):
            if item in list(collection):
                collection.remove(item)
                break

    ctx.bump()
    return {"group": ids.ensure_id(group), "removed": name, "revision": ctx.revision}


def _find_interface_item(group, identifier: str):
    if compatibility.uses_tree_interface() and hasattr(group, "interface"):
        for item in group.interface.items_tree:
            if getattr(item, "item_type", "SOCKET") != "SOCKET":
                continue
            if item.identifier == identifier or item.name == identifier:
                return item
    else:
        for collection in (getattr(group, "inputs", []), getattr(group, "outputs", [])):
            for socket in collection:
                if socket.identifier == identifier or socket.name == identifier:
                    return socket
    inputs, outputs = _interface_sockets(group)
    raise BridgeError(
        ErrorCode.INVALID_NODE_SOCKET,
        f"`{group.name}` has no interface socket `{identifier}`.",
        {"requested": identifier, "inputs": inputs, "outputs": outputs},
    )


# --- graph editing ---------------------------------------------------------


@read("geometry_nodes.tree.get")
def get_tree(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    return {"tree": n.summarise_tree(tree, args, domain="geometry"), "revision": ctx.revision}


@read("geometry_nodes.node.list")
def list_nodes(ctx, args: dict) -> dict[str, Any]:
    from .shader import list_nodes as shared

    return shared(ctx, args)


@read("geometry_nodes.node.get")
def get_node(ctx, args: dict) -> dict[str, Any]:
    from .shader import get_node as shared

    return shared(ctx, args)


@op("geometry_nodes.node.create")
def create_node(ctx, args: dict) -> dict[str, Any]:
    from .shader import create_node as shared

    return shared(ctx, args)


@op("geometry_nodes.node.update")
def update_node(ctx, args: dict) -> dict[str, Any]:
    from .shader import update_node as shared

    return shared(ctx, args)


@op("geometry_nodes.node.delete")
def delete_node(ctx, args: dict) -> dict[str, Any]:
    from .shader import delete_node as shared

    return shared(ctx, args)


@read("geometry_nodes.link.list")
def list_links(ctx, args: dict) -> dict[str, Any]:
    from .shader import list_links as shared

    return shared(ctx, args)


@op("geometry_nodes.link.create")
def create_link(ctx, args: dict) -> dict[str, Any]:
    from .shader import create_link as shared

    return shared(ctx, args)


@op("geometry_nodes.link.delete")
def delete_link(ctx, args: dict) -> dict[str, Any]:
    from .shader import delete_link as shared

    return shared(ctx, args)


@op("geometry_nodes.graph.build")
def build_graph(ctx, args: dict) -> dict[str, Any]:
    """Apply a declarative graph plan.

    The plan is a list of nodes, each with a caller-chosen `key`, and a list of
    links referring to those keys. The whole plan is applied in one pass, which
    is what lets the server compute a scatter or an array in Rust and hand the
    result over as data rather than as a sequence of round trips.
    """
    tree, label = n.resolve_tree(args)
    nodes = c.optional_list(args, "nodes")
    links = c.optional_list(args, "links")
    clear = c.optional_bool(args, "clear", False)

    if not nodes:
        raise invalid_argument("`nodes` must contain at least one node.", field="nodes")

    if clear:
        for node in list(tree.nodes):
            tree.nodes.remove(node)

    created: dict[str, Any] = {}
    for spec in nodes:
        if not isinstance(spec, dict):
            raise invalid_argument("Each entry in `nodes` must be an object.", field="nodes")
        key = str(spec.get("key") or spec.get("name") or "")
        if not key:
            raise invalid_argument("Each node needs a `key` to be referenced by links.")
        if key in created:
            raise invalid_argument(f"Duplicate node key `{key}`.", key=key)
        node_type = str(spec["node_type"])
        created[key] = n.create_node(tree, node_type, spec)

    built_links = []
    for link in links:
        from_key = str(link["from"]["node"])
        to_key = str(link["to"]["node"])
        from_node = created.get(from_key) or n.find_node(tree, from_key)
        to_node = created.get(to_key) or n.find_node(tree, to_key)
        from_socket = n.resolve_socket(from_node, _selector(link["from"]), "output")
        to_socket = n.resolve_socket(to_node, _selector(link["to"]), "input")
        built_links.append(n.summarise_link(n.link_sockets(tree, from_socket, to_socket)))

    ctx.bump()
    return {
        "tree": label,
        "nodes": {
            key: n.summarise_node(node, include_sockets=False, include_ui=True)
            for key, node in created.items()
        },
        "links": built_links,
        "revision": ctx.revision,
    }


def _selector(payload: dict) -> dict:
    for key in ("identifier", "index", "name"):
        if key in payload:
            return {key: payload[key]}
    raise invalid_argument("A socket reference needs `identifier`, `index` or `name`.")


# --- modifier attachment ---------------------------------------------------


@op("geometry_nodes.modifier.attach")
def attach(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    group = group_arg(args)
    name = c.optional_str(args, "modifier_name") or group.name

    modifier = obj.modifiers.new(name=name, type="NODES")
    modifier.node_group = group

    applied = []
    for entry in c.optional_list(args, "inputs"):
        socket_name = str(entry["name"])
        identifier = _input_identifier(group, socket_name)
        try:
            modifier[identifier] = n.decode_value(entry["value"], f"input `{socket_name}`")
        except (KeyError, TypeError, ValueError) as error:
            raise BridgeError(
                ErrorCode.INVALID_NODE_SOCKET,
                f"Could not set group input `{socket_name}`: {error}",
                {"socket": socket_name, "identifier": identifier},
            ) from error
        applied.append(socket_name)

    ctx.bump()
    return {
        "object": ids.ensure_id(obj),
        "modifier": modifier.name,
        "group": ids.ensure_id(group),
        "inputs_set": applied,
        "revision": ctx.revision,
    }


def _input_identifier(group, name: str) -> str:
    inputs, _outputs = _interface_sockets(group)
    for entry in inputs:
        if entry["name"] == name or entry["identifier"] == name:
            return entry["identifier"]
    raise BridgeError(
        ErrorCode.INVALID_NODE_SOCKET,
        f"`{group.name}` has no group input `{name}`.",
        {"requested": name, "available": [entry["name"] for entry in inputs]},
    )


@op("geometry_nodes.modifier.detach")
def detach(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    name = c.optional_str(args, "modifier")

    candidates = [m for m in obj.modifiers if m.type == "NODES"]
    if not candidates:
        raise BridgeError(
            ErrorCode.MODIFIER_NOT_FOUND,
            f"`{obj.name}` has no geometry nodes modifier.",
            {"object": obj.name},
        )
    if name is None:
        if len(candidates) > 1:
            raise invalid_argument(
                f"`{obj.name}` has {len(candidates)} geometry nodes modifiers; name the one to "
                "detach.",
                modifiers=[m.name for m in candidates],
            )
        modifier = candidates[0]
    else:
        modifier = next((m for m in candidates if m.name == name), None)
        if modifier is None:
            raise BridgeError(
                ErrorCode.MODIFIER_NOT_FOUND,
                f"`{obj.name}` has no geometry nodes modifier `{name}`.",
                {"available": [m.name for m in candidates]},
            )

    removed = modifier.name
    obj.modifiers.remove(modifier)
    ctx.bump()
    return {"object": ids.ensure_id(obj), "removed": removed, "revision": ctx.revision}


@read("geometry_nodes.modifier.list")
def list_modifiers(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    entries = [
        {
            "modifier": modifier.name,
            "group": ids.ensure_id(modifier.node_group) if modifier.node_group else None,
            "group_name": modifier.node_group.name if modifier.node_group else None,
            "show_viewport": bool(modifier.show_viewport),
        }
        for modifier in obj.modifiers
        if modifier.type == "NODES"
    ]
    return {
        "object": ids.ensure_id(obj),
        "modifiers": entries,
        "total": len(entries),
        "revision": ctx.revision,
    }

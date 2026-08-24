"""Shader node graph operations.

Generic graph editing, not a fixed set of presets: any registered shader node
type can be created, configured and wired. The higher-level material workflows
are built on top of these same operations, server-side.
"""

from __future__ import annotations

from typing import Any

import bpy

from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from . import _nodes as n


@read("shader.tree.get")
def get_tree(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    return {"tree": n.summarise_tree(tree, args, domain="shader"), "revision": ctx.revision}


@op("shader.tree.clear")
def clear_tree(ctx, args: dict) -> dict[str, Any]:
    tree, label = n.resolve_tree(args)
    keep_output = c.optional_bool(args, "keep_output", True)

    removed = 0
    for node in list(tree.nodes):
        if keep_output and node.type in {"OUTPUT_MATERIAL", "OUTPUT_WORLD"}:
            continue
        tree.nodes.remove(node)
        removed += 1

    ctx.bump()
    return {"cleared": removed, "tree": label, "revision": ctx.revision}


@read("shader.node.list")
def list_nodes(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    nodes = sorted(tree.nodes, key=lambda node: node.name)
    window, cursor = c.paginate(nodes, args)
    return {
        "nodes": [
            n.summarise_node(node, include_sockets=False, include_ui=True) for node in window
        ],
        "total": len(nodes),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("shader.node.get")
def get_node(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    node = n.find_node(tree, c.require_str(args, "node"))
    return {
        "node": n.summarise_node(
            node, include_defaults=True, include_properties=True, include_ui=True
        ),
        "revision": ctx.revision,
    }


@op("shader.node.create")
def create_node(ctx, args: dict) -> dict[str, Any]:
    tree, label = n.resolve_tree(args)
    node_type = c.require_str(args, "node_type")
    node = n.create_node(tree, node_type, args)
    ctx.bump()
    return {
        "node": n.summarise_node(node, include_defaults=True, include_ui=True),
        "tree": label,
        "revision": ctx.revision,
    }


@op("shader.node.update")
def update_node(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    node = n.find_node(tree, c.require_str(args, "node"))
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        node.label = name
        node.name = name
        changed.append("name")

    location = c.optional(args, "location")
    if location is not None:
        node.location = (float(location.get("x", 0.0)), float(location.get("y", 0.0)))
        changed.append("location")

    mute = c.optional_bool(args, "mute")
    if mute is not None:
        node.mute = mute
        changed.append("mute")

    for assignment in c.optional_list(args, "properties"):
        n.set_property(node, str(assignment["name"]), assignment["value"])
        changed.append(f"property:{assignment['name']}")

    for default in c.optional_list(args, "inputs"):
        socket = n.resolve_socket(node, _selector(default), "input")
        n.set_socket_default(socket, default["value"])
        changed.append(f"input:{socket.identifier}")

    if not changed:
        raise invalid_argument("Nothing to update on this node.")

    ctx.bump()
    return {
        "node": n.summarise_node(node, include_defaults=True, include_ui=True),
        "changed": changed,
        "revision": ctx.revision,
    }


def _selector(payload: dict) -> dict:
    for key in ("identifier", "index", "name"):
        if key in payload:
            return {key: payload[key]}
    raise invalid_argument("A socket reference needs `identifier`, `index` or `name`.")


@op("shader.node.delete")
def delete_node(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    node = n.find_node(tree, c.require_str(args, "node"))
    if node.type in {"OUTPUT_MATERIAL", "OUTPUT_WORLD"} and not c.optional_bool(
        args, "force", False
    ):
        raise invalid_argument(
            f"`{node.name}` is the tree output; deleting it makes the material render as flat "
            "black. Pass force:true if that is genuinely intended.",
            node=node.name,
        )
    payload = {"id": n.ensure_node_id(node), "name": node.name, "type": node.bl_idname}
    tree.nodes.remove(node)
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


@read("shader.link.list")
def list_links(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    links = [n.summarise_link(link) for link in tree.links]
    return {"links": links, "total": len(links), "revision": ctx.revision}


@op("shader.link.create")
def create_link(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    from_address = c.require(args, "from")
    to_address = c.require(args, "to")
    replace = c.optional_bool(args, "replace_existing", True)

    from_node = n.find_node(tree, str(from_address["node"]))
    to_node = n.find_node(tree, str(to_address["node"]))
    from_socket = n.resolve_socket(from_node, _selector(from_address), "output")
    to_socket = n.resolve_socket(to_node, _selector(to_address), "input")

    link = n.link_sockets(tree, from_socket, to_socket, replace_existing=bool(replace))
    ctx.bump()
    return {"link": n.summarise_link(link), "revision": ctx.revision}


@op("shader.link.delete")
def delete_link(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    to_address = c.optional(args, "to")
    from_address = c.optional(args, "from")

    if to_address is None and from_address is None:
        raise invalid_argument("Specify `to`, `from`, or both.")

    removed = 0
    for link in list(tree.links):
        matches = True
        if to_address is not None:
            node = n.find_node(tree, str(to_address["node"]))
            socket = n.resolve_socket(node, _selector(to_address), "input")
            matches = matches and link.to_socket == socket
        if from_address is not None:
            node = n.find_node(tree, str(from_address["node"]))
            socket = n.resolve_socket(node, _selector(from_address), "output")
            matches = matches and link.from_socket == socket
        if matches:
            tree.links.remove(link)
            removed += 1

    ctx.bump()
    return {"removed": removed, "revision": ctx.revision}


@read("shader.socket.get")
def get_socket(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    node = n.find_node(tree, c.require_str(args, "node"))
    direction = c.enum_value(
        c.optional_str(args, "direction", "input") or "input", ["input", "output"], "direction"
    )
    socket = n.resolve_socket(node, _selector(args), direction)
    sockets = node.inputs if direction == "input" else node.outputs
    index = next(i for i, s in enumerate(sockets) if s == socket)
    return {
        "socket": n.summarise_socket(socket, index, include_default=True),
        "node": n.ensure_node_id(node),
        "revision": ctx.revision,
    }


@op("shader.socket.set_default")
def set_socket_default(ctx, args: dict) -> dict[str, Any]:
    tree, _label = n.resolve_tree(args)
    node = n.find_node(tree, c.require_str(args, "node"))
    socket = n.resolve_socket(node, _selector(args), "input")
    if socket.is_linked and not c.optional_bool(args, "force", False):
        raise invalid_argument(
            f"`{socket.name}` is driven by a link, so its default value has no effect. "
            "Delete the link first, or pass force:true to set the value anyway.",
            node=node.name,
            socket=socket.identifier,
        )
    n.set_socket_default(socket, c.require(args, "value"))
    ctx.bump()
    index = next(i for i, s in enumerate(node.inputs) if s == socket)
    return {
        "socket": n.summarise_socket(socket, index, include_default=True),
        "revision": ctx.revision,
    }


@op("shader.graph.build")
def build_graph(ctx, args: dict) -> dict[str, Any]:
    """Apply a declarative graph plan to a shader tree.

    The same operation as `geometry_nodes.graph.build` -- a node tree is a node
    tree -- registered under a shader-side name so the material workflows read
    as what they are.
    """
    from .geometry_nodes import build_graph as shared

    return shared(ctx, args)

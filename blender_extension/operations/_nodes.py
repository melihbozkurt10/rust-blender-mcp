"""Shared node-graph machinery for shader, world and geometry trees.

Node trees are uniform enough across Blender that one implementation covers all
three. What differs -- which node types exist, where the tree lives -- is
handled by the tree resolver and by capability checks in the Rust server.

Socket addressing is the delicate part. Display names are not unique (a Mix
node has two sockets called ``A``) and several were renamed in Blender 4.0, so
a name that resolves to more than one socket produces a structured error
listing the candidates rather than a silent wrong pick.
"""

from __future__ import annotations

from typing import Any

import bpy

from .. import ids
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

#: Custom property carrying a node's stable id. Nodes are not ID data-blocks,
#: but they do support custom properties, so the same scheme works.
NODE_ID = "mcp_node_id"


# --- tree resolution -------------------------------------------------------


def resolve_tree(args: dict) -> tuple[Any, str]:
    """Find the node tree an operation targets.

    Returns the tree and a human-readable label for error messages. The tagged
    union the protocol sends is one of ``material``, ``node_tree``,
    ``object_modifier`` or ``world``.
    """
    if "material" in args and args["material"] is not None:
        material = ids.find_material(str(args["material"]))
        if not material.use_nodes:
            material.use_nodes = True
        return material.node_tree, f"material `{material.name}`"

    if "node_tree" in args and args["node_tree"] is not None:
        tree = ids.find("node_tree", str(args["node_tree"]))
        return tree, f"node group `{tree.name}`"

    if "object_modifier" in args and args["object_modifier"] is not None:
        spec = args["object_modifier"]
        if not isinstance(spec, dict):
            raise invalid_argument("`object_modifier` must be an object.", field="object_modifier")
        obj = ids.find_object(str(spec.get("object", "")))
        modifier_name = spec.get("modifier")
        modifiers = [m for m in obj.modifiers if m.type == "NODES"]
        if not modifiers:
            raise BridgeError(
                ErrorCode.MODIFIER_NOT_FOUND,
                f"`{obj.name}` has no geometry nodes modifier.",
                {"object": obj.name},
            )
        if modifier_name is None:
            if len(modifiers) > 1:
                raise invalid_argument(
                    f"`{obj.name}` has {len(modifiers)} geometry nodes modifiers; name the one "
                    "to use.",
                    object=obj.name,
                    modifiers=[m.name for m in modifiers],
                )
            modifier = modifiers[0]
        else:
            modifier = next((m for m in modifiers if m.name == modifier_name), None)
            if modifier is None:
                raise BridgeError(
                    ErrorCode.MODIFIER_NOT_FOUND,
                    f"`{obj.name}` has no geometry nodes modifier named `{modifier_name}`.",
                    {"object": obj.name, "available": [m.name for m in modifiers]},
                )
        if modifier.node_group is None:
            raise BridgeError(
                ErrorCode.NODE_TREE_NOT_FOUND,
                f"Modifier `{modifier.name}` has no node group assigned.",
                {"object": obj.name, "modifier": modifier.name},
            )
        return modifier.node_group, f"modifier `{modifier.name}` on `{obj.name}`"

    if args.get("world") is not None or "world" in args:
        world = bpy.context.scene.world
        if world is None:
            world = bpy.data.worlds.new("World")
            bpy.context.scene.world = world
        world.use_nodes = True
        return world.node_tree, "the world"

    raise invalid_argument(
        "Name the tree to operate on: `material`, `node_tree`, `object_modifier` or `world`."
    )


# --- node identity ---------------------------------------------------------


def ensure_node_id(node) -> str:
    """The node stable id, assigning one if needed.

    Not every node accepts a custom property. Nodes inside a group appended
    from another .blend can be library data, and some node types raise rather
    than store one -- some add-on shader groups hit both. A node we cannot tag
    still has to be reportable, so it falls back to its name, which is unique
    within a tree even if it does not survive a rename.
    """
    try:
        existing = node.get(NODE_ID)
    except (KeyError, TypeError, AttributeError):
        return node.name
    if isinstance(existing, str) and existing:
        return existing
    import uuid

    new_id = str(uuid.uuid4())
    try:
        node[NODE_ID] = new_id
    except (KeyError, TypeError, AttributeError, RuntimeError):
        return node.name
    return new_id


def find_node(tree, reference: str):
    """Resolve a node by stable id or by name."""
    if not isinstance(reference, str) or not reference:
        raise invalid_argument("A node reference must be a non-empty string.")

    for node in tree.nodes:
        if node.get(NODE_ID) == reference:
            return node

    node = tree.nodes.get(reference)
    if node is not None:
        return node

    # Labels are what a human sees in the editor, so accept them last.
    labelled = [n for n in tree.nodes if n.label == reference]
    if len(labelled) == 1:
        return labelled[0]
    if len(labelled) > 1:
        raise BridgeError(
            ErrorCode.NODE_NOT_FOUND,
            f"`{reference}` matches {len(labelled)} nodes by label; use a node id.",
            {"reference": reference, "matches": [n.name for n in labelled]},
        )

    raise BridgeError(
        ErrorCode.NODE_NOT_FOUND,
        f"No node matches `{reference}`.",
        {
            "reference": reference,
            "available": [
                {"id": ensure_node_id(n), "name": n.name, "type": n.bl_idname}
                for n in list(tree.nodes)[:30]
            ],
        },
    )


# --- socket addressing -----------------------------------------------------


def resolve_socket(node, selector: dict, direction: str):
    """Resolve a socket from the protocol tagged selector.

    ``selector`` is one of ``{"identifier": ...}``, ``{"index": ...}`` or
    ``{"name": ...}``.
    """
    sockets = node.inputs if direction == "input" else node.outputs

    if not isinstance(selector, dict) or not selector:
        raise BridgeError(
            ErrorCode.INVALID_NODE_SOCKET,
            "A socket selector must be one of identifier, index or name.",
            {"node": node.name},
        )

    if "index" in selector:
        index = selector["index"]
        if not isinstance(index, int) or index < 0 or index >= len(sockets):
            raise BridgeError(
                ErrorCode.INVALID_NODE_SOCKET,
                f"`{node.name}` has {len(sockets)} {direction} sockets; index {index} is out of range.",
                {
                    "node": node.name,
                    "requested_index": index,
                    "socket_count": len(sockets),
                    **_available(sockets, direction),
                },
            )
        return sockets[index]

    if "identifier" in selector:
        wanted = str(selector["identifier"])
        for socket in sockets:
            if socket.identifier == wanted:
                return socket
        # Identifiers and names coincide for most nodes, so try names before
        # giving up -- a caller that used a display name here still gets the
        # socket it meant.
        matches = [s for s in sockets if s.name == wanted]
        if len(matches) == 1:
            return matches[0]
        raise _no_such_socket(node, wanted, sockets, direction)

    if "name" in selector:
        wanted = str(selector["name"])
        matches = [s for s in sockets if s.name == wanted]
        if len(matches) == 1:
            return matches[0]
        if not matches:
            # Blender shows a display name but scripts usually know the
            # identifier, and the two differ on several nodes (the Noise
            # texture output is displayed as `Factor` but identified as `Fac`).
            # Accepting either is strictly more useful and stays unambiguous,
            # because identifiers are unique within a node.
            by_identifier = [s for s in sockets if s.identifier == wanted]
            if len(by_identifier) == 1:
                return by_identifier[0]
        if len(matches) > 1:
            raise BridgeError(
                ErrorCode.INVALID_NODE_SOCKET,
                f"`{node.bl_idname}` has {len(matches)} {direction} sockets named `{wanted}`. "
                "Use `identifier` or `index` to say which.",
                {
                    "node": node.name,
                    "requested_socket": wanted,
                    "candidates": [
                        {"identifier": s.identifier, "index": i, "type": s.type}
                        for i, s in enumerate(sockets)
                        if s.name == wanted
                    ],
                },
            )
        raise _no_such_socket(node, wanted, sockets, direction)

    raise BridgeError(
        ErrorCode.INVALID_NODE_SOCKET,
        "A socket selector must be one of identifier, index or name.",
        {"node": node.name, "given": sorted(selector)},
    )


def _available(sockets, direction: str) -> dict[str, Any]:
    key = "available_inputs" if direction == "input" else "available_outputs"
    return {
        key: [
            {"identifier": s.identifier, "name": s.name, "index": i, "type": s.type}
            for i, s in enumerate(sockets)
        ]
    }


def _no_such_socket(node, wanted: str, sockets, direction: str) -> BridgeError:
    return BridgeError(
        ErrorCode.INVALID_NODE_SOCKET,
        f"`{wanted}` is not an {direction} socket on {node.bl_idname}.",
        {
            "node": node.name,
            "node_type": node.bl_idname,
            "requested_socket": wanted,
            **_available(sockets, direction),
        },
    )


# --- property and value handling -------------------------------------------

#: Attributes that must never be written, whatever the caller asks for.
FORBIDDEN_PROPERTIES = frozenset(
    {
        "bl_idname",
        "bl_rna",
        "rna_type",
        "id_data",
        "type",
        "inputs",
        "outputs",
        "internal_links",
        "parent",
    }
)


def set_property(node, name: str, value: dict) -> None:
    """Set one node property from the protocol tagged value.

    The name must be a real, writable RNA property of this node. There is no
    path by which a caller-supplied string reaches ``setattr`` without passing
    that check first.
    """
    if name in FORBIDDEN_PROPERTIES or name.startswith("_"):
        raise BridgeError(
            ErrorCode.PERMISSION_DENIED,
            f"`{name}` is not a settable node property.",
            {"property": name},
        )

    rna = node.bl_rna.properties.get(name)
    if rna is None:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"`{node.bl_idname}` has no property `{name}`.",
            {
                "node_type": node.bl_idname,
                "requested": name,
                "available": sorted(_settable_property_names(node)),
            },
        )
    if rna.is_readonly:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"`{name}` is read-only on {node.bl_idname}.",
            {"node_type": node.bl_idname, "property": name},
        )

    decoded = decode_value(value, f"property `{name}`")

    if rna.type == "ENUM":
        allowed = [item.identifier for item in rna.enum_items]
        if decoded not in allowed:
            raise BridgeError(
                ErrorCode.INVALID_ENUM,
                f"`{decoded}` is not a valid `{name}` on {node.bl_idname}.",
                {"property": name, "value": decoded, "allowed": allowed},
            )

    try:
        setattr(node, name, decoded)
    except (TypeError, ValueError) as error:
        raise BridgeError(
            ErrorCode.INVALID_PROPERTY,
            f"Could not set `{name}` on {node.bl_idname}: {error}",
            {"property": name, "node_type": node.bl_idname},
        ) from error


def _settable_property_names(node) -> list[str]:
    return [
        prop.identifier
        for prop in node.bl_rna.properties
        if not prop.is_readonly and prop.identifier not in FORBIDDEN_PROPERTIES
    ]


def decode_value(value: dict, field: str):
    """Turn a tagged protocol value into something ``bpy`` accepts."""
    if not isinstance(value, dict) or len(value) != 1:
        raise invalid_argument(
            f"{field} must be a tagged value such as {{\"float\": 0.5}}.", field=field
        )
    kind, payload = next(iter(value.items()))

    if kind == "bool":
        return bool(payload)
    if kind == "int":
        return int(payload)
    if kind == "float":
        return c.check_finite(float(payload), field)
    if kind == "string":
        return str(payload)
    if kind == "enum":
        return str(payload)
    if kind == "vec2":
        return list(c.as_vector({**payload, "z": 0.0}, field))[:2]
    if kind == "vec3":
        return list(c.as_vector(payload, field))
    if kind == "color":
        return c.as_color(payload, field)
    if kind == "image":
        return ids.find("image", str(payload))
    if kind == "object":
        return ids.find_object(str(payload))
    if kind == "material":
        return ids.find_material(str(payload))
    if kind == "collection":
        return ids.find_collection(str(payload))
    if kind == "node_group":
        return ids.find("node_tree", str(payload))

    raise invalid_argument(
        f"{field} has unknown value kind `{kind}`.",
        field=field,
        kind=kind,
    )


def set_socket_default(socket, value: dict) -> None:
    """Write a socket default, coercing to whatever shape the socket wants."""
    if not hasattr(socket, "default_value"):
        raise BridgeError(
            ErrorCode.INVALID_NODE_SOCKET,
            f"Socket `{socket.name}` ({socket.type}) has no default value to set. "
            "Shader and geometry sockets only carry data through links.",
            {"socket": socket.identifier, "socket_type": socket.type},
        )

    decoded = decode_value(value, f"socket `{socket.name}`")
    current = socket.default_value

    try:
        if hasattr(current, "__len__") and not isinstance(current, str):
            width = len(current)
            values = decoded if isinstance(decoded, (list, tuple)) else [decoded] * width
            if len(values) < width:
                # A colour socket asked for RGBA but given RGB: alpha is opaque.
                values = list(values) + [1.0] * (width - len(values))
            socket.default_value = list(values)[:width]
        else:
            socket.default_value = decoded
    except (TypeError, ValueError) as error:
        raise BridgeError(
            ErrorCode.INVALID_NODE_SOCKET,
            f"Could not set the default on `{socket.name}` ({socket.type}): {error}",
            {"socket": socket.identifier, "socket_type": socket.type},
        ) from error


def encode_value(value) -> dict | None:
    """Turn a bpy value back into a tagged protocol value."""
    if isinstance(value, bool):
        return {"bool": value}
    if isinstance(value, int):
        return {"int": value}
    if isinstance(value, float):
        return {"float": value}
    if isinstance(value, str):
        return {"string": value}
    if isinstance(value, bpy.types.Image):
        return {"image": ids.ensure_id(value)}
    if isinstance(value, bpy.types.Object):
        return {"object": ids.ensure_id(value)}
    if isinstance(value, bpy.types.Material):
        return {"material": ids.ensure_id(value)}
    if isinstance(value, bpy.types.Collection):
        return {"collection": ids.ensure_id(value)}
    if isinstance(value, bpy.types.NodeTree):
        return {"node_group": ids.ensure_id(value)}
    if hasattr(value, "__len__"):
        components = [float(v) for v in value]
        if len(components) == 2:
            return {"vec2": {"x": components[0], "y": components[1]}}
        if len(components) == 3:
            return {"vec3": {"x": components[0], "y": components[1], "z": components[2]}}
        if len(components) == 4:
            return {
                "color": {
                    "r": components[0],
                    "g": components[1],
                    "b": components[2],
                    "a": components[3],
                }
            }
    return None


# --- serialisation ---------------------------------------------------------


def summarise_socket(socket, index: int, include_default: bool) -> dict[str, Any]:
    payload = {
        "identifier": socket.identifier,
        "name": socket.name,
        "index": index,
        "type": socket.type,
        "is_linked": bool(socket.is_linked),
    }
    if include_default and hasattr(socket, "default_value"):
        encoded = encode_value(socket.default_value)
        if encoded is not None:
            payload["default_value"] = encoded
    return payload


def summarise_node(
    node,
    *,
    include_sockets: bool = True,
    include_defaults: bool = False,
    include_properties: bool = False,
    include_ui: bool = False,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "id": ensure_node_id(node),
        "name": node.name,
        "type": node.bl_idname,
        "mute": bool(node.mute),
    }
    if include_ui:
        payload["label"] = node.label or None
        payload["location"] = {"x": float(node.location[0]), "y": float(node.location[1])}
        payload["width"] = float(node.width)
    if include_sockets:
        payload["inputs"] = [
            summarise_socket(socket, index, include_defaults)
            for index, socket in enumerate(node.inputs)
        ]
        payload["outputs"] = [
            summarise_socket(socket, index, include_defaults)
            for index, socket in enumerate(node.outputs)
        ]
    if include_properties:
        payload["properties"] = _node_properties(node)
    return payload


def _node_properties(node) -> list[dict[str, Any]]:
    """The node type-specific properties, excluding the ones every node has."""
    common = set(bpy.types.Node.bl_rna.properties.keys())
    out = []
    for prop in node.bl_rna.properties:
        if prop.identifier in common or prop.is_readonly:
            continue
        try:
            value = getattr(node, prop.identifier)
        except AttributeError:
            continue
        encoded = encode_value(value)
        if encoded is None and prop.type == "ENUM":
            encoded = {"enum": str(value)}
        if encoded is not None:
            out.append({"name": prop.identifier, "value": encoded})
    return out


def summarise_link(link) -> dict[str, Any]:
    return {
        "from_node": ensure_node_id(link.from_node),
        "from_socket": link.from_socket.identifier,
        "to_node": ensure_node_id(link.to_node),
        "to_socket": link.to_socket.identifier,
        "is_valid": bool(link.is_valid),
    }


def summarise_tree(tree, args: dict, domain: str) -> dict[str, Any]:
    include_defaults = bool(args.get("include_socket_defaults", False))
    include_ui = bool(args.get("include_ui_metadata", False))
    include_properties = bool(args.get("include_properties", False))
    wanted = args.get("nodes") or []

    nodes = list(tree.nodes)
    if wanted:
        selected = {find_node(tree, str(reference)).name for reference in wanted}
        nodes = [node for node in nodes if node.name in selected]

    node_names = {node.name for node in nodes}
    links = [
        link
        for link in tree.links
        if link.from_node.name in node_names and link.to_node.name in node_names
    ]

    return {
        "domain": domain,
        "name": tree.name,
        "nodes": [
            summarise_node(
                node,
                include_defaults=include_defaults,
                include_properties=include_properties,
                include_ui=include_ui,
            )
            for node in nodes
        ],
        "links": [summarise_link(link) for link in links],
    }


# --- graph editing ---------------------------------------------------------


def create_node(tree, node_type: str, args: dict):
    """Create a node, with a clear error when the type is not registered."""
    try:
        node = tree.nodes.new(type=node_type)
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.INVALID_NODE_TYPE,
            f"`{node_type}` is not a node type available in this tree: {error}",
            {"node_type": node_type, "tree": tree.name},
        ) from error

    name = args.get("name")
    if name:
        node.label = str(name)
        node.name = str(name)

    location = args.get("location")
    if location is not None:
        node.location = (float(location.get("x", 0.0)), float(location.get("y", 0.0)))

    for assignment in args.get("properties") or []:
        set_property(node, str(assignment["name"]), assignment["value"])

    for default in args.get("inputs") or []:
        socket = resolve_socket(node, _selector_of(default), "input")
        set_socket_default(socket, default["value"])

    ensure_node_id(node)
    return node


def _selector_of(payload: dict) -> dict:
    """Pull the flattened socket selector out of a socket-default payload."""
    for key in ("identifier", "index", "name"):
        if key in payload:
            return {key: payload[key]}
    raise invalid_argument(
        "A socket default needs `identifier`, `index` or `name`.",
    )


def link_sockets(tree, from_socket, to_socket, replace_existing: bool = True):
    """Connect two sockets, optionally clearing what was there."""
    if replace_existing:
        for link in [link for link in tree.links if link.to_socket == to_socket]:
            tree.links.remove(link)
    try:
        return tree.links.new(from_socket, to_socket)
    except (RuntimeError, TypeError) as error:
        raise BridgeError(
            ErrorCode.INVALID_NODE_SOCKET,
            f"Blender refused the link {from_socket.name} -> {to_socket.name}: {error}",
            {
                "from_socket": from_socket.identifier,
                "from_type": from_socket.type,
                "to_socket": to_socket.identifier,
                "to_type": to_socket.type,
            },
        ) from error


def find_output_node(tree, kinds: tuple[str, ...]):
    """The tree's output node, preferring the active one."""
    candidates = [node for node in tree.nodes if node.type in kinds]
    if not candidates:
        return None
    active = [node for node in candidates if getattr(node, "is_active_output", False)]
    return active[0] if active else candidates[0]

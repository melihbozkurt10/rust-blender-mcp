"""Material operations, including Principled BSDF handling."""

from __future__ import annotations

from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from . import _nodes as n

#: Semantic Principled inputs and how their values are decoded.
PRINCIPLED_SCALARS = (
    "metallic",
    "roughness",
    "ior",
    "alpha",
    "emission_strength",
    "specular",
    "transmission",
    "coat_weight",
    "coat_roughness",
    "sheen_weight",
    "sheen_roughness",
    "anisotropic",
    "anisotropic_rotation",
)
PRINCIPLED_COLORS = ("base_color", "emission_color")


def principled_node(material):
    """The material's Principled BSDF, or ``None``."""
    if not material.use_nodes or material.node_tree is None:
        return None
    output = n.find_output_node(material.node_tree, ("OUTPUT_MATERIAL",))
    if output is not None:
        surface = output.inputs.get("Surface")
        if surface is not None and surface.is_linked:
            candidate = surface.links[0].from_node
            if candidate.type == "BSDF_PRINCIPLED":
                return candidate
    # Fall back to any Principled in the tree: a material still being built up
    # may not be wired to the output yet.
    for node in material.node_tree.nodes:
        if node.type == "BSDF_PRINCIPLED":
            return node
    return None


def require_principled(material):
    node = principled_node(material)
    if node is None:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{material.name}` has no Principled BSDF. Edit its node graph directly with the "
            "shader tools, or recreate the material with `use_nodes: true`.",
            {"material": material.name},
        )
    return node


def read_principled(material) -> dict[str, Any] | None:
    node = principled_node(material)
    if node is None:
        return None
    payload: dict[str, Any] = {}
    for semantic in PRINCIPLED_COLORS:
        socket = compatibility.principled_socket(node, semantic)
        if socket is not None:
            payload[semantic] = c.color_dict(socket.default_value)
    for semantic in PRINCIPLED_SCALARS:
        socket = compatibility.principled_socket(node, semantic)
        if socket is not None:
            try:
                payload[semantic] = float(socket.default_value)
            except TypeError:
                # A socket that is driven by a link has no scalar default worth
                # reporting.
                continue
    normal = compatibility.principled_socket(node, "normal")
    if normal is not None and normal.is_linked:
        source = normal.links[0].from_node
        if source.type == "NORMAL_MAP":
            payload["normal_strength"] = float(source.inputs["Strength"].default_value)
    return payload


def write_principled(material, values: dict) -> list[str]:
    """Apply semantic Principled values. Returns which ones were written."""
    node = require_principled(material)
    written: list[str] = []

    for semantic in PRINCIPLED_COLORS:
        if values.get(semantic) is None:
            continue
        socket = compatibility.require_principled_socket(node, semantic)
        socket.default_value = c.as_color(values[semantic], semantic)
        written.append(semantic)

    for semantic in PRINCIPLED_SCALARS:
        if values.get(semantic) is None:
            continue
        socket = compatibility.require_principled_socket(node, semantic)
        socket.default_value = float(values[semantic])
        written.append(semantic)

    strength = values.get("normal_strength")
    if strength is not None:
        normal = compatibility.principled_socket(node, "normal")
        if normal is None or not normal.is_linked:
            raise BridgeError(
                ErrorCode.UNSUPPORTED_PROPERTY,
                "`normal_strength` needs a normal map connected to the Principled BSDF.",
                {"material": material.name},
            )
        source = normal.links[0].from_node
        if source.type != "NORMAL_MAP":
            raise BridgeError(
                ErrorCode.UNSUPPORTED_PROPERTY,
                f"The Normal input is driven by a {source.bl_idname}, not a Normal Map node, "
                "so there is no strength to set.",
                {"material": material.name, "node_type": source.bl_idname},
            )
        source.inputs["Strength"].default_value = float(strength)
        written.append("normal_strength")

    return written


def apply_settings(material, settings: dict) -> list[str]:
    written: list[str] = []
    blend = settings.get("blend_method")
    if blend is not None:
        # Blender 4.2 replaced `blend_method` with `surface_render_method` for
        # EEVEE Next; older builds still use the old property.
        if hasattr(material, "surface_render_method"):
            material.surface_render_method = "BLENDED" if blend != "OPAQUE" else "DITHERED"
            written.append("surface_render_method")
        elif hasattr(material, "blend_method"):
            material.blend_method = blend
            written.append("blend_method")
        else:
            raise BridgeError(
                ErrorCode.UNSUPPORTED_PROPERTY,
                "This Blender build exposes neither `blend_method` nor `surface_render_method`.",
                {"requested": blend},
            )
    if settings.get("use_backface_culling") is not None:
        material.use_backface_culling = bool(settings["use_backface_culling"])
        written.append("use_backface_culling")
    if settings.get("displacement_method") is not None:
        if hasattr(material, "displacement_method"):
            material.displacement_method = settings["displacement_method"]
            written.append("displacement_method")
    if settings.get("viewport_color") is not None:
        material.diffuse_color = c.as_color(settings["viewport_color"], "viewport_color")
        written.append("viewport_color")
    if settings.get("alpha_threshold") is not None and hasattr(material, "alpha_threshold"):
        material.alpha_threshold = float(settings["alpha_threshold"])
        written.append("alpha_threshold")
    return written


def read_settings(material) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "use_backface_culling": bool(material.use_backface_culling),
        "viewport_color": c.color_dict(material.diffuse_color),
    }
    for attribute, key in (
        ("blend_method", "blend_method"),
        ("surface_render_method", "blend_method"),
        ("displacement_method", "displacement_method"),
        ("alpha_threshold", "alpha_threshold"),
    ):
        if hasattr(material, attribute):
            value = getattr(material, attribute)
            payload[key] = float(value) if isinstance(value, float) else str(value)
    return payload


def summarise(material, *, detail: bool = False) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "id": ids.ensure_id(material),
        "name": material.name,
        "use_nodes": bool(material.use_nodes),
        "users": int(material.users),
        "node_count": len(material.node_tree.nodes) if material.use_nodes and material.node_tree else 0,
    }
    principled = read_principled(material)
    if principled:
        payload["principled"] = principled
    images = _referenced_images(material)
    if images:
        payload["images"] = images
    if detail:
        payload["settings"] = read_settings(material)
        payload["fake_user"] = bool(material.use_fake_user)
    return payload


def _referenced_images(material) -> list[str]:
    if not material.use_nodes or material.node_tree is None:
        return []
    found = []
    for node in material.node_tree.nodes:
        image = getattr(node, "image", None)
        if image is not None:
            found.append(image.name)
    return sorted(set(found))


# --- operations ------------------------------------------------------------


@read("material.list")
def list_materials(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    used_by = c.optional_str(args, "used_by")
    unused = c.optional_bool(args, "unused")

    candidates = list(bpy.data.materials)
    if used_by is not None:
        obj = ids.find_object(used_by)
        names = {slot.material.name for slot in obj.material_slots if slot.material}
        candidates = [m for m in candidates if m.name in names]
    if unused is not None:
        candidates = [m for m in candidates if (m.users == 0) == unused]

    matched = [m for m in candidates if c.matches_name(m.name, name_filter)]
    matched.sort(key=lambda m: m.name)
    window, cursor = c.paginate(matched, args)
    return {
        "materials": [summarise(m) for m in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("material.get")
def get(ctx, args: dict) -> dict[str, Any]:
    material = c.material_arg(args)
    return {"material": summarise(material, detail=True), "revision": ctx.revision}


@op("material.create")
def create(ctx, args: dict) -> dict[str, Any]:
    name = c.require_str(args, "name")
    use_nodes = c.optional_bool(args, "use_nodes", True)

    material = bpy.data.materials.new(name)
    material.use_nodes = bool(use_nodes)

    principled = c.optional(args, "principled")
    if principled:
        write_principled(material, principled)
    settings = c.optional(args, "settings")
    if settings:
        apply_settings(material, settings)

    assigned = []
    for obj in c.objects_arg(args, "assign_to", required=False):
        slot_index = _assign(obj, material, slot_index=None, replace_all=False)
        assigned.append({"object": ids.ensure_id(obj), "slot_index": slot_index})

    ids.invalidate_cache("material")
    ctx.bump()
    return {
        "material": summarise(material, detail=True),
        "assigned": assigned,
        "revision": ctx.revision,
    }


@op("material.update")
def update(ctx, args: dict) -> dict[str, Any]:
    material = c.material_arg(args)
    changed: list[str] = []

    name = c.optional_str(args, "name")
    if name is not None:
        material.name = name
        changed.append("name")
        ids.invalidate_cache("material")

    principled = c.optional(args, "principled")
    if principled:
        changed.extend(write_principled(material, principled))

    settings = c.optional(args, "settings")
    if settings:
        changed.extend(apply_settings(material, settings))

    if not changed:
        raise invalid_argument("Nothing to update.")

    ctx.bump()
    return {"material": summarise(material, detail=True), "changed": changed, "revision": ctx.revision}


@op("material.duplicate")
def duplicate(ctx, args: dict) -> dict[str, Any]:
    material = c.material_arg(args)
    name = c.optional_str(args, "name")
    copy = material.copy()
    from .. import config

    # A copied data-block inherits the id; a duplicate is a new entity.
    copy.pop(config.ID_PROPERTY, None)
    if name:
        copy.name = name
    ids.invalidate_cache("material")
    ctx.bump()
    return {"material": summarise(copy, detail=True), "revision": ctx.revision}


@op("material.delete")
def delete(ctx, args: dict) -> dict[str, Any]:
    material = c.material_arg(args)
    force = c.optional_bool(args, "force", False)
    if material.users > 0 and not force:
        raise invalid_argument(
            f"`{material.name}` is used by {material.users} data-block(s). "
            "Unassign it first, or pass force:true to delete it anyway.",
            material=material.name,
            users=material.users,
        )
    payload = {"id": ids.ensure_id(material), "name": material.name}
    bpy.data.materials.remove(material)
    ids.invalidate_cache("material")
    ctx.bump()
    return {"deleted": payload, "revision": ctx.revision}


@op("material.assign")
def assign(ctx, args: dict) -> dict[str, Any]:
    material = c.material_arg(args)
    objects = c.objects_arg(args, "objects")
    slot_index = c.optional_int(args, "slot_index")
    replace_all = c.optional_bool(args, "replace_all", False)
    face_indices = [int(i) for i in c.optional_list(args, "face_indices")]
    expected_revision = c.optional_int(args, "expected_mesh_revision")

    results = []
    for obj in objects:
        if face_indices:
            index = _assign_faces(obj, material, face_indices, expected_revision)
        else:
            index = _assign(obj, material, slot_index, replace_all)
        results.append(
            {
                "object_id": ids.ensure_id(obj),
                "material_id": ids.ensure_id(material),
                "slot_index": index,
            }
        )

    ctx.bump()
    return {"assignments": results, "revision": ctx.revision}


def _assign(obj, material, slot_index: int | None, replace_all: bool) -> int:
    if not hasattr(obj.data, "materials"):
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{obj.name}` ({obj.type}) cannot hold materials.",
            {"object": obj.name, "type": obj.type},
        )

    if replace_all:
        obj.data.materials.clear()
        obj.data.materials.append(material)
        return 0

    if slot_index is not None:
        if slot_index < 0 or slot_index >= len(obj.material_slots):
            raise invalid_argument(
                f"`{obj.name}` has {len(obj.material_slots)} material slot(s); "
                f"index {slot_index} is out of range.",
                object=obj.name,
                slot_count=len(obj.material_slots),
            )
        obj.material_slots[slot_index].material = material
        return slot_index

    # Reuse an empty slot before adding another: appending blindly leaves
    # objects with a growing list of unused slots.
    for index, slot in enumerate(obj.material_slots):
        if slot.material is None:
            slot.material = material
            return index
        if slot.material == material:
            return index

    obj.data.materials.append(material)
    return len(obj.material_slots) - 1


def _assign_faces(obj, material, face_indices: list[int], expected_revision: int | None) -> int:
    mesh = c.require_mesh(obj)
    ids.check_mesh_revision(mesh, expected_revision)

    slot_index = _assign(obj, material, None, False)
    face_count = len(mesh.polygons)
    out_of_range = [i for i in face_indices if i < 0 or i >= face_count]
    if out_of_range:
        raise invalid_argument(
            f"`{obj.name}` has {face_count} faces; {len(out_of_range)} of the given indices are "
            "out of range.",
            object=obj.name,
            face_count=face_count,
            out_of_range=out_of_range[:20],
        )
    for index in face_indices:
        mesh.polygons[index].material_index = slot_index
    mesh.update()
    return slot_index


@op("material.unassign")
def unassign(ctx, args: dict) -> dict[str, Any]:
    objects = c.objects_arg(args, "objects")
    material = c.material_arg(args, "material", required=False)
    remove_slot = c.optional_bool(args, "remove_slot", True)

    cleared = []
    for obj in objects:
        for index in reversed(range(len(obj.material_slots))):
            slot = obj.material_slots[index]
            if material is not None and slot.material != material:
                continue
            slot.material = None
            cleared.append({"object": ids.ensure_id(obj), "slot_index": index})
            if remove_slot and hasattr(obj.data, "materials") and index < len(obj.data.materials):
                obj.data.materials.pop(index=index)
    ctx.bump()
    return {"cleared": cleared, "revision": ctx.revision}


@read("material.slot.list")
def list_slots(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    return {
        "object_id": ids.ensure_id(obj),
        "slots": [
            {
                "index": index,
                "material": ids.ensure_id(slot.material) if slot.material else None,
                "name": slot.material.name if slot.material else None,
                "link": slot.link,
            }
            for index, slot in enumerate(obj.material_slots)
        ],
        "revision": ctx.revision,
    }

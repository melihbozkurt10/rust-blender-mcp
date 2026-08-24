"""Texture baking.

Baking is Cycles-only in Blender. Where Cycles is not available -- it is an
add-on, and a build can ship without it -- that is reported as a capability
problem rather than as a mysterious operator failure.
"""

from __future__ import annotations

import os
import time
from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import external
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

BAKE_TYPES = [
    "COMBINED",
    "AO",
    "SHADOW",
    "POSITION",
    "NORMAL",
    "UV",
    "ROUGHNESS",
    "EMIT",
    "ENVIRONMENT",
    "DIFFUSE",
    "GLOSSY",
    "TRANSMISSION",
]

#: Passes whose output is data rather than colour, and so must not be
#: colour-managed on the way out.
DATA_PASSES = {"NORMAL", "ROUGHNESS", "POSITION", "UV", "AO"}


def _require_cycles(scene) -> None:
    engines = compatibility.available_render_engines()
    if "CYCLES" not in engines:
        raise BridgeError(
            ErrorCode.CAPABILITY_UNAVAILABLE,
            "Baking needs Cycles, which is not available in this Blender build. Enable the Cycles "
            "add-on in Preferences, or start Blender without --factory-startup.",
            {"available_engines": engines},
        )


@external("texture.bake")
def bake(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    _require_cycles(scene)

    target = c.object_arg(args, "target")
    mesh = c.require_mesh(target)
    sources = c.objects_arg(args, "sources", required=False)
    bake_type = c.enum_value(c.require_str(args, "type"), BAKE_TYPES, "type")
    width = c.optional_int(args, "width", 1024) or 1024
    height = c.optional_int(args, "height", 1024) or 1024
    margin = c.optional_int(args, "margin", 16) or 0
    samples = c.optional_int(args, "samples")
    output_path = c.require_str(args, "output_path")
    connect = c.optional_bool(args, "connect_to_material", False)

    if not os.path.isabs(output_path):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`output_path` must be absolute; the server supplies it.",
            {"output_path": output_path},
        )
    if target in sources:
        raise invalid_argument("The bake target cannot also be a source.")
    if not mesh.uv_layers:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{target.name}` has no UV map, so there is nowhere to bake to. Unwrap it first.",
            {"object": target.name},
        )

    uv_map = c.optional_str(args, "uv_map")
    if uv_map is not None:
        layer = mesh.uv_layers.get(uv_map)
        if layer is None:
            raise invalid_argument(
                f"`{target.name}` has no UV map `{uv_map}`.",
                available=[l.name for l in mesh.uv_layers],
            )
        mesh.uv_layers.active = layer

    if not target.data.materials:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            f"`{target.name}` has no material; a bake needs one to write the image node into.",
            {"object": target.name},
        )

    image = bpy.data.images.new(
        c.optional_str(args, "name") or f"{target.name}_{bake_type.lower()}",
        width=width,
        height=height,
        alpha=True,
        float_buffer=bake_type in DATA_PASSES,
        is_data=bake_type in DATA_PASSES,
    )

    # Every material on the target needs an image node selected and active, or
    # Cycles has nowhere to write.
    created_nodes = []
    for slot in target.material_slots:
        material = slot.material
        if material is None:
            continue
        material.use_nodes = True
        node = material.node_tree.nodes.new("ShaderNodeTexImage")
        node.image = image
        node.label = "MCP Bake Target"
        node.select = True
        material.node_tree.nodes.active = node
        created_nodes.append((material, node))

    previous_engine = scene.render.engine
    cycles = getattr(scene, "cycles", None)
    previous_samples = getattr(cycles, "samples", None) if cycles else None

    started = time.monotonic()
    try:
        scene.render.engine = "CYCLES"
        if samples is not None and cycles is not None:
            cycles.samples = samples

        scene.render.bake.margin = margin
        scene.render.bake.use_selected_to_active = bool(sources)
        if sources:
            extrusion = c.optional_float(args, "cage_extrusion")
            if extrusion is not None:
                scene.render.bake.cage_extrusion = extrusion
            max_ray = c.optional_float(args, "max_ray_distance")
            if max_ray is not None:
                scene.render.bake.max_ray_distance = max_ray
            cage_reference = c.optional_str(args, "cage_object")
            if cage_reference is not None:
                scene.render.bake.cage_object = ids.find_object(cage_reference)

        with c.object_mode(target):
            for source in sources:
                source.select_set(True)
            try:
                bpy.ops.object.bake(type=bake_type)
            except RuntimeError as error:
                raise BridgeError(
                    ErrorCode.BLENDER_INTERNAL_ERROR,
                    f"The bake failed: {error}",
                    {"type": bake_type, "object": target.name},
                ) from error

        image.filepath_raw = output_path
        image.file_format = c.optional_str(args, "format", "PNG") or "PNG"
        image.save()
        size = os.path.getsize(output_path) if os.path.exists(output_path) else 0
    finally:
        scene.render.engine = previous_engine
        if cycles is not None and previous_samples is not None:
            cycles.samples = previous_samples
        if not connect:
            for material, node in created_nodes:
                material.node_tree.nodes.remove(node)

    if connect:
        _connect_baked_image(created_nodes, bake_type)

    ids.invalidate_cache("image")
    ctx.bump()
    return {
        "files": [{"path": output_path, "size_bytes": size, "frame": scene.frame_current}],
        "image": summarise(image),
        "type": bake_type,
        "width": width,
        "height": height,
        "duration_ms": int((time.monotonic() - started) * 1000),
        "connected": bool(connect),
        "revision": ctx.revision,
    }


def summarise(image) -> dict[str, Any]:
    from .uv import summarise_image

    return summarise_image(image)


def _connect_baked_image(created_nodes, bake_type: str) -> None:
    """Wire a baked map into the Principled BSDF it came from."""
    from .material import principled_node

    socket_for = {
        "DIFFUSE": "base_color",
        "COMBINED": "base_color",
        "ROUGHNESS": "roughness",
        "EMIT": "emission_color",
    }
    semantic = socket_for.get(bake_type)

    for material, node in created_nodes:
        bsdf = principled_node(material)
        if bsdf is None:
            continue
        tree = material.node_tree
        if bake_type == "NORMAL":
            normal_map = tree.nodes.new("ShaderNodeNormalMap")
            normal_map.location = (node.location.x + 200, node.location.y)
            tree.links.new(node.outputs["Color"], normal_map.inputs["Color"])
            socket = compatibility.principled_socket(bsdf, "normal")
            if socket is not None:
                tree.links.new(normal_map.outputs["Normal"], socket)
            continue
        if semantic is None:
            continue
        socket = compatibility.principled_socket(bsdf, semantic)
        if socket is not None:
            tree.links.new(node.outputs["Color"], socket)

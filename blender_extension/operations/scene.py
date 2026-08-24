"""Scene, world, selection and system operations."""

from __future__ import annotations

from typing import Any

import bpy
from mathutils import Vector

from .. import compatibility, ids
from ..dispatcher import HANDLERS, OP_KINDS, op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c
from .object import summarise as summarise_object

# --- system ----------------------------------------------------------------


@read("system.capabilities")
def capabilities(ctx, args: dict) -> dict[str, Any]:
    """What this Blender build can do. Re-queried on demand, not just at connect."""
    return {
        "identity": compatibility.identity(),
        "capabilities": compatibility.capabilities(),
        "operations": sorted(HANDLERS),
        "revision": ctx.revision,
    }


@read("system.operations")
def operations(ctx, args: dict) -> dict[str, Any]:
    """Every operation this add-on implements, with its side-effect class."""
    return {
        "operations": [
            {"op": name, "kind": OP_KINDS.get(name, "WRITE")} for name in sorted(HANDLERS)
        ],
        "count": len(HANDLERS),
    }


@read("system.ping")
def ping(ctx, args: dict) -> dict[str, Any]:
    return {"pong": True, "revision": ctx.revision}


# --- scene -----------------------------------------------------------------


@read("scene.summary")
def summary(ctx, args: dict) -> dict[str, Any]:
    """The compact state a model should read before doing anything else."""
    scene = bpy.context.scene
    view_layer = bpy.context.view_layer

    counts = {"total": 0, "mesh": 0, "light": 0, "camera": 0, "armature": 0, "curve": 0, "empty": 0, "other": 0}
    for obj in scene.objects:
        counts["total"] += 1
        key = {
            "MESH": "mesh",
            "LIGHT": "light",
            "CAMERA": "camera",
            "ARMATURE": "armature",
            "CURVE": "curve",
            "EMPTY": "empty",
        }.get(obj.type, "other")
        counts[key] += 1

    selected = [obj.name for obj in view_layer.objects if obj.select_get()]
    active = view_layer.objects.active

    return {
        "revision": ctx.revision,
        "scene": scene.name,
        "scene_id": ids.ensure_id(scene),
        "objects": counts,
        "materials": len(bpy.data.materials),
        "collections": len(bpy.data.collections),
        "images": len(bpy.data.images),
        "actions": len(bpy.data.actions),
        "selected": selected[:50],
        "active_object": active.name if active is not None else None,
        "active_camera": scene.camera.name if scene.camera is not None else None,
        "frame_current": scene.frame_current,
        "frame_start": scene.frame_start,
        "frame_end": scene.frame_end,
        "fps": scene.render.fps / max(scene.render.fps_base, 1e-6),
        "render_engine": scene.render.engine,
        "unit_scale": scene.unit_settings.scale_length,
        "filepath": bpy.data.filepath or None,
        "unsaved_changes": bool(bpy.data.is_dirty),
    }


@read("scene.get")
def get_scene(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    render = scene.render
    return {
        "scene": {
            "id": ids.ensure_id(scene),
            "name": scene.name,
            "frame_start": scene.frame_start,
            "frame_end": scene.frame_end,
            "frame_current": scene.frame_current,
            "frame_step": scene.frame_step,
            "fps": render.fps / max(render.fps_base, 1e-6),
            "unit_system": scene.unit_settings.system,
            "unit_scale": scene.unit_settings.scale_length,
            "gravity": c.vector_dict(scene.gravity),
            "cursor_location": c.vector_dict(scene.cursor.location),
            "active_camera": scene.camera.name if scene.camera else None,
            "render_engine": render.engine,
            "resolution": {"x": render.resolution_x, "y": render.resolution_y},
            "resolution_percentage": render.resolution_percentage,
            "world": scene.world.name if scene.world else None,
            "collections": [child.name for child in scene.collection.children],
        },
        "revision": ctx.revision,
    }


@read("scene.settings.get")
def get_settings(ctx, args: dict) -> dict[str, Any]:
    return get_scene(ctx, args)


@op("scene.settings.update")
def update_settings(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    changed: list[str] = []

    frame_start = c.optional_int(args, "frame_start")
    frame_end = c.optional_int(args, "frame_end")
    if frame_start is not None:
        scene.frame_start = frame_start
        changed.append("frame_start")
    if frame_end is not None:
        scene.frame_end = frame_end
        changed.append("frame_end")
    frame_current = c.optional_int(args, "frame_current")
    if frame_current is not None:
        scene.frame_set(frame_current)
        changed.append("frame_current")

    fps = c.optional_float(args, "fps")
    if fps is not None:
        # Blender stores fps as an integer numerator over fps_base; setting
        # 23.976 as an integer silently becomes 23.
        if abs(fps - round(fps)) < 1e-6:
            scene.render.fps = int(round(fps))
            scene.render.fps_base = 1.0
        else:
            scene.render.fps = int(round(fps))
            scene.render.fps_base = scene.render.fps / fps
        changed.append("fps")

    unit_scale = c.optional_float(args, "unit_scale")
    if unit_scale is not None:
        scene.unit_settings.scale_length = unit_scale
        changed.append("unit_scale")

    unit_system = c.optional_str(args, "unit_system")
    if unit_system is not None:
        scene.unit_settings.system = c.enum_value(
            unit_system, ["NONE", "METRIC", "IMPERIAL"], "unit_system"
        )
        changed.append("unit_system")

    active_camera = c.optional_str(args, "active_camera")
    if active_camera is not None:
        camera = ids.find_object(active_camera)
        if camera.type != "CAMERA":
            raise invalid_argument(
                f"`{camera.name}` is a {camera.type} object, not a camera.",
                field="active_camera",
            )
        scene.camera = camera
        changed.append("active_camera")

    cursor = c.optional_vector(args, "cursor_location")
    if cursor is not None:
        scene.cursor.location = cursor
        changed.append("cursor_location")

    gravity = c.optional_vector(args, "gravity")
    if gravity is not None:
        scene.gravity = gravity
        changed.append("gravity")

    if not changed:
        raise invalid_argument("No settings were provided.")

    ctx.bump()
    return {"changed": changed, "revision": ctx.revision}


# --- world -----------------------------------------------------------------


@read("scene.world.get")
def get_world(ctx, args: dict) -> dict[str, Any]:
    world = bpy.context.scene.world
    if world is None:
        return {"world": None, "revision": ctx.revision}
    payload: dict[str, Any] = {
        "id": ids.ensure_id(world),
        "name": world.name,
        "use_nodes": bool(world.use_nodes),
    }
    background = _background_node(world)
    if background is not None:
        payload["color"] = c.color_dict(background.inputs["Color"].default_value)
        payload["strength"] = float(background.inputs["Strength"].default_value)
        environment = _environment_texture(world)
        if environment is not None and environment.image is not None:
            payload["hdri"] = environment.image.filepath or environment.image.name
    elif not world.use_nodes:
        payload["color"] = c.color_dict(world.color)
    payload["transparent"] = bool(bpy.context.scene.render.film_transparent)
    return {"world": payload, "revision": ctx.revision}


def _background_node(world):
    if not world.use_nodes or world.node_tree is None:
        return None
    for node in world.node_tree.nodes:
        if node.type == "BACKGROUND":
            return node
    return None


def _environment_texture(world):
    if not world.use_nodes or world.node_tree is None:
        return None
    for node in world.node_tree.nodes:
        if node.type == "TEX_ENVIRONMENT":
            return node
    return None


@op("scene.world.update")
def update_world(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    world = scene.world
    if world is None:
        world = bpy.data.worlds.new("World")
        scene.world = world
    world.use_nodes = True

    tree = world.node_tree
    background = _background_node(world)
    if background is None:
        background = tree.nodes.new("ShaderNodeBackground")
        output = next((n for n in tree.nodes if n.type == "OUTPUT_WORLD"), None)
        if output is None:
            output = tree.nodes.new("ShaderNodeOutputWorld")
        tree.links.new(background.outputs["Background"], output.inputs["Surface"])

    changed: list[str] = []

    color = c.optional(args, "color")
    if color is not None:
        background.inputs["Color"].default_value = c.as_color(color, "color")
        changed.append("color")

    strength = c.optional_float(args, "strength")
    if strength is not None:
        background.inputs["Strength"].default_value = strength
        changed.append("strength")

    hdri = c.optional_str(args, "hdri")
    if hdri is not None:
        _attach_hdri(tree, background, hdri, c.optional_float(args, "rotation_z"))
        changed.append("hdri")
    elif c.optional_float(args, "rotation_z") is not None:
        environment = _environment_texture(world)
        if environment is None:
            raise invalid_argument(
                "`rotation_z` needs an environment texture; set `hdri` as well.",
                field="rotation_z",
            )
        _set_environment_rotation(tree, environment, c.optional_float(args, "rotation_z") or 0.0)
        changed.append("rotation_z")

    transparent = c.optional_bool(args, "transparent")
    if transparent is not None:
        scene.render.film_transparent = transparent
        changed.append("transparent")

    if not changed:
        raise invalid_argument("No world settings were provided.")

    ctx.bump()
    return {"changed": changed, "world": ids.ensure_id(world), "revision": ctx.revision}


def _attach_hdri(tree, background, image_reference: str, rotation_z: float | None) -> None:
    image = _resolve_image(image_reference)
    environment = next((n for n in tree.nodes if n.type == "TEX_ENVIRONMENT"), None)
    if environment is None:
        environment = tree.nodes.new("ShaderNodeTexEnvironment")
        environment.location = (background.location.x - 400, background.location.y)
    environment.image = image
    tree.links.new(environment.outputs["Color"], background.inputs["Color"])
    if rotation_z is not None:
        _set_environment_rotation(tree, environment, rotation_z)


def _set_environment_rotation(tree, environment, rotation_z: float) -> None:
    mapping = next((n for n in tree.nodes if n.type == "MAPPING"), None)
    if mapping is None:
        mapping = tree.nodes.new("ShaderNodeMapping")
        mapping.location = (environment.location.x - 200, environment.location.y)
        coordinates = next((n for n in tree.nodes if n.type == "TEX_COORD"), None)
        if coordinates is None:
            coordinates = tree.nodes.new("ShaderNodeTexCoord")
            coordinates.location = (mapping.location.x - 200, mapping.location.y)
        tree.links.new(coordinates.outputs["Generated"], mapping.inputs["Vector"])
        tree.links.new(mapping.outputs["Vector"], environment.inputs["Vector"])
    rotation = mapping.inputs["Rotation"].default_value
    mapping.inputs["Rotation"].default_value = (rotation[0], rotation[1], rotation_z)


def _resolve_image(reference: str):
    image = ids.find("image", reference, required=False)
    if image is not None:
        return image
    raise BridgeError(
        ErrorCode.IMAGE_NOT_FOUND,
        f"No loaded image matches `{reference}`. Load it with `image.load` first.",
        {"reference": reference, "loaded": [i.name for i in bpy.data.images][:20]},
    )


# --- selection -------------------------------------------------------------


@read("selection.get")
def get_selection(ctx, args: dict) -> dict[str, Any]:
    view_layer = bpy.context.view_layer
    selected = [obj for obj in view_layer.objects if obj.select_get()]
    active = view_layer.objects.active
    return {
        "selected": [ids.ensure_id(obj) for obj in selected],
        "names": [obj.name for obj in selected],
        "active": ids.ensure_id(active) if active is not None else None,
        "mode": bpy.context.mode,
        "revision": ctx.revision,
    }


@op("selection.set")
def set_selection(ctx, args: dict) -> dict[str, Any]:
    return _update_selection(ctx, args, default_mode="SET")


@op("selection.add")
def add_selection(ctx, args: dict) -> dict[str, Any]:
    return _update_selection(ctx, args, default_mode="ADD")


@op("selection.remove")
def remove_selection(ctx, args: dict) -> dict[str, Any]:
    return _update_selection(ctx, args, default_mode="REMOVE")


def _update_selection(ctx, args: dict, default_mode: str) -> dict[str, Any]:
    mode = c.enum_value(
        c.optional_str(args, "mode", default_mode) or default_mode,
        ["SET", "ADD", "REMOVE"],
        "mode",
    )
    view_layer = bpy.context.view_layer
    objects = c.objects_arg(args, "objects", required=False)

    if mode == "SET":
        for obj in view_layer.objects:
            obj.select_set(False)

    for obj in objects:
        if obj.name not in view_layer.objects:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"`{obj.name}` is not in the active view layer and cannot be selected.",
                {"object": obj.name},
            )
        obj.select_set(mode != "REMOVE")

    active_reference = c.optional_str(args, "active")
    if active_reference is not None:
        active = ids.find_object(active_reference)
        if active.name not in view_layer.objects:
            raise BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                f"`{active.name}` is not in the active view layer and cannot be made active.",
                {"object": active.name},
            )
        active.select_set(True)
        view_layer.objects.active = active

    ctx.bump()
    return get_selection(ctx, {})


@op("selection.clear")
def clear_selection(ctx, args: dict) -> dict[str, Any]:
    view_layer = bpy.context.view_layer
    for obj in view_layer.objects:
        obj.select_set(False)
    view_layer.objects.active = None
    ctx.bump()
    return {"selected": [], "names": [], "active": None, "revision": ctx.revision}


@op("selection.set_active")
def set_active(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    view_layer = bpy.context.view_layer
    if obj.name not in view_layer.objects:
        raise BridgeError(
            ErrorCode.BLENDER_CONTEXT_ERROR,
            f"`{obj.name}` is not in the active view layer.",
            {"object": obj.name},
        )
    obj.select_set(True)
    view_layer.objects.active = obj
    ctx.bump()
    return {"active": ids.ensure_id(obj), "name": obj.name, "revision": ctx.revision}


# --- statistics ------------------------------------------------------------


@read("scene.statistics")
def statistics(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    counts = {"total": 0, "mesh": 0, "light": 0, "camera": 0, "armature": 0, "curve": 0, "empty": 0, "other": 0}
    vertices = edges = faces = triangles = modifiers = hidden = 0

    for obj in scene.objects:
        counts["total"] += 1
        key = {
            "MESH": "mesh",
            "LIGHT": "light",
            "CAMERA": "camera",
            "ARMATURE": "armature",
            "CURVE": "curve",
            "EMPTY": "empty",
        }.get(obj.type, "other")
        counts[key] += 1
        modifiers += len(obj.modifiers)
        if obj.hide_viewport or obj.hide_render:
            hidden += 1
        if obj.type == "MESH" and obj.data is not None:
            mesh = obj.data
            vertices += len(mesh.vertices)
            edges += len(mesh.edges)
            faces += len(mesh.polygons)
            triangles += sum(max(len(p.vertices) - 2, 0) for p in mesh.polygons)

    texture_bytes = 0
    for image in bpy.data.images:
        width, height = image.size[0], image.size[1]
        # 4 channels at 4 bytes each is Blender's float buffer; byte-depth
        # images use a quarter of that. This is an estimate and is labelled as
        # one.
        depth = 16 if image.is_float else 4
        texture_bytes += width * height * depth

    return {
        "objects": counts,
        "vertices": vertices,
        "edges": edges,
        "faces": faces,
        "triangles": triangles,
        "materials": len(bpy.data.materials),
        "images": len(bpy.data.images),
        "collections": len(bpy.data.collections),
        "modifiers": modifiers,
        "hidden_objects": hidden,
        "texture_memory_bytes": texture_bytes,
        "revision": ctx.revision,
    }


@read("scene.snapshot")
def snapshot(ctx, args: dict) -> dict[str, Any]:
    """The current revision, for a caller that wants to diff against it later."""
    return {"revision": ctx.revision}

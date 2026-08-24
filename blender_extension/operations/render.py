"""Rendering and viewport capture.

Output paths are decided by the Rust server, which owns the managed render
directory. The bridge is handed an absolute path and writes there; it never
constructs one from caller input, so there is no place for a traversal to
happen on this side.
"""

from __future__ import annotations

import os
import time
from typing import Any

import bpy

from .. import compatibility, ids
from ..dispatcher import external, op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument, invalid_enum
from . import _common as c
from .camera import camera_arg

ENGINE_ALIASES = {"CYCLES": "CYCLES", "EEVEE": "EEVEE", "WORKBENCH": "WORKBENCH"}


def _cycles(scene):
    """Cycles settings, or ``None`` when the add-on is not enabled."""
    return getattr(scene, "cycles", None)


def read_settings(scene) -> dict[str, Any]:
    render = scene.render
    payload: dict[str, Any] = {
        "engine": render.engine,
        "resolution_x": render.resolution_x,
        "resolution_y": render.resolution_y,
        "resolution_percentage": render.resolution_percentage,
        "transparent_background": bool(render.film_transparent),
        "format": render.image_settings.file_format,
        "color_mode": render.image_settings.color_mode,
        "quality": int(render.image_settings.quality),
        "fps": render.fps / max(render.fps_base, 1e-6),
        "frame_current": scene.frame_current,
        "view_transform": scene.view_settings.view_transform,
        "film_exposure": float(scene.view_settings.exposure),
        "motion_blur": bool(getattr(render, "use_motion_blur", False)),
    }
    cycles = _cycles(scene)
    if cycles is not None:
        payload["samples"] = int(getattr(cycles, "samples", 0))
        payload["adaptive_threshold"] = float(getattr(cycles, "adaptive_threshold", 0.0))
        payload["denoise"] = bool(getattr(cycles, "use_denoising", False))
        payload["max_bounces"] = int(getattr(cycles, "max_bounces", 0))
    eevee = getattr(scene, "eevee", None)
    if eevee is not None and hasattr(eevee, "taa_render_samples"):
        payload.setdefault("samples", int(eevee.taa_render_samples))
    return payload


def apply_settings(scene, args: dict) -> tuple[list[str], dict[str, Any]]:
    """Apply render settings, returning what changed and how to undo it."""
    render = scene.render
    changed: list[str] = []
    previous: dict[str, Any] = {}

    def remember(key: str, value: Any) -> None:
        previous.setdefault(key, value)

    engine = c.optional_str(args, "engine")
    if engine is not None:
        remember("engine", render.engine)
        render.engine = compatibility.resolve_engine(
            c.enum_value(engine, sorted(ENGINE_ALIASES), "engine")
        )
        changed.append("engine")

    for key, attribute in (
        ("resolution_x", "resolution_x"),
        ("resolution_y", "resolution_y"),
        ("resolution_percentage", "resolution_percentage"),
    ):
        value = c.optional_int(args, key)
        if value is not None:
            remember(key, getattr(render, attribute))
            setattr(render, attribute, value)
            changed.append(key)

    transparent = c.optional_bool(args, "transparent_background")
    if transparent is not None:
        remember("transparent_background", render.film_transparent)
        render.film_transparent = transparent
        changed.append("transparent_background")

    image_format = c.optional_str(args, "format")
    if image_format is not None:
        remember("format", render.image_settings.file_format)
        render.image_settings.file_format = image_format
        changed.append("format")

    color_mode = c.optional_str(args, "color_mode")
    if color_mode is not None:
        remember("color_mode", render.image_settings.color_mode)
        render.image_settings.color_mode = c.enum_value(
            color_mode, ["BW", "RGB", "RGBA"], "color_mode"
        )
        changed.append("color_mode")

    quality = c.optional_int(args, "quality")
    if quality is not None:
        remember("quality", render.image_settings.quality)
        render.image_settings.quality = quality
        changed.append("quality")

    view_transform = c.optional_str(args, "view_transform")
    if view_transform is not None:
        remember("view_transform", scene.view_settings.view_transform)
        try:
            scene.view_settings.view_transform = view_transform
        except TypeError as error:
            available = [
                item.identifier
                for item in bpy.types.ColorManagedViewSettings.bl_rna.properties[
                    "view_transform"
                ].enum_items
            ]
            raise BridgeError(
                ErrorCode.INVALID_ENUM,
                f"`{view_transform}` is not a view transform in this build.",
                {"value": view_transform, "allowed": available},
            ) from error
        changed.append("view_transform")

    exposure = c.optional_float(args, "film_exposure")
    if exposure is not None:
        remember("film_exposure", scene.view_settings.exposure)
        scene.view_settings.exposure = exposure
        changed.append("film_exposure")

    motion_blur = c.optional_bool(args, "motion_blur")
    if motion_blur is not None and hasattr(render, "use_motion_blur"):
        remember("motion_blur", render.use_motion_blur)
        render.use_motion_blur = motion_blur
        changed.append("motion_blur")

    samples = c.optional_int(args, "samples")
    if samples is not None:
        cycles = _cycles(scene)
        if render.engine == "CYCLES" and cycles is not None:
            remember("samples", cycles.samples)
            cycles.samples = samples
        else:
            eevee = getattr(scene, "eevee", None)
            if eevee is None or not hasattr(eevee, "taa_render_samples"):
                raise BridgeError(
                    ErrorCode.UNSUPPORTED_PROPERTY,
                    "This build exposes no sample count for the active engine.",
                    {"engine": render.engine},
                )
            remember("samples", eevee.taa_render_samples)
            eevee.taa_render_samples = samples
        changed.append("samples")

    cycles = _cycles(scene)
    if cycles is not None:
        threshold = c.optional_float(args, "adaptive_threshold")
        if threshold is not None:
            remember("adaptive_threshold", cycles.adaptive_threshold)
            cycles.adaptive_threshold = threshold
            changed.append("adaptive_threshold")
        denoise = c.optional_bool(args, "denoise")
        if denoise is not None:
            remember("denoise", cycles.use_denoising)
            cycles.use_denoising = denoise
            changed.append("denoise")
        bounces = c.optional_int(args, "max_bounces")
        if bounces is not None:
            remember("max_bounces", cycles.max_bounces)
            cycles.max_bounces = bounces
            changed.append("max_bounces")
        use_gpu = c.optional_bool(args, "use_gpu")
        if use_gpu is not None:
            remember("device", cycles.device)
            cycles.device = "GPU" if use_gpu else "CPU"
            changed.append("use_gpu")

    return changed, previous


def restore_settings(scene, previous: dict[str, Any]) -> None:
    """Put back whatever `apply_settings` changed."""
    render = scene.render
    cycles = _cycles(scene)
    eevee = getattr(scene, "eevee", None)
    for key, value in previous.items():
        try:
            if key == "engine":
                render.engine = value
            elif key in {"resolution_x", "resolution_y", "resolution_percentage"}:
                setattr(render, key, value)
            elif key == "transparent_background":
                render.film_transparent = value
            elif key == "format":
                render.image_settings.file_format = value
            elif key == "color_mode":
                render.image_settings.color_mode = value
            elif key == "quality":
                render.image_settings.quality = value
            elif key == "view_transform":
                scene.view_settings.view_transform = value
            elif key == "film_exposure":
                scene.view_settings.exposure = value
            elif key == "motion_blur":
                render.use_motion_blur = value
            elif key == "samples":
                if cycles is not None and render.engine == "CYCLES":
                    cycles.samples = value
                elif eevee is not None:
                    eevee.taa_render_samples = value
            elif key in {"adaptive_threshold", "denoise", "max_bounces", "device"} and cycles:
                setattr(
                    cycles,
                    {"denoise": "use_denoising"}.get(key, key),
                    value,
                )
        except (AttributeError, TypeError, ValueError) as error:
            # Restoration is best effort: an engine switch can make a setting
            # unavailable, and failing here would mask the render result.
            print(f"[blender-mcp] could not restore `{key}`: {error}")


@read("render.settings.get")
def get_settings(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    return {
        "settings": read_settings(scene),
        "available_engines": compatibility.available_render_engines(),
        "revision": ctx.revision,
    }


@op("render.settings.update")
def update_settings(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    changed, _previous = apply_settings(scene, args)
    if not changed:
        raise invalid_argument("No render settings were provided.")
    ctx.bump()
    return {"changed": changed, "settings": read_settings(scene), "revision": ctx.revision}


@op("render.engine.set")
def set_engine(ctx, args: dict) -> dict[str, Any]:
    scene = bpy.context.scene
    engine = c.enum_value(c.require_str(args, "engine"), sorted(ENGINE_ALIASES), "engine")
    resolved = compatibility.resolve_engine(engine)
    scene.render.engine = resolved
    ctx.bump()
    return {"engine": resolved, "requested": engine, "revision": ctx.revision}


@external("render.execute")
def execute(ctx, args: dict) -> dict[str, Any]:
    """Render to a path the server chose.

    `output_path` is always supplied by the Rust server and always lands inside
    a managed root. The bridge does not invent paths.
    """
    scene = bpy.context.scene
    output_path = c.require_str(args, "output_path")
    if not os.path.isabs(output_path):
        raise BridgeError(
            ErrorCode.INVALID_PATH,
            "`output_path` must be absolute; the server supplies it.",
            {"output_path": output_path},
        )

    camera = camera_arg(args)
    previous_camera = scene.camera
    scene.camera = camera

    restore = c.optional_bool(args, "restore_settings", True)
    changed, previous = apply_settings(scene, args)

    frames = _frames_to_render(scene, args)
    previous_frame = scene.frame_current
    previous_output = scene.render.filepath

    # Read the output size now: `restore_settings` runs in the `finally` below,
    # so by the time the result is built the scene is back to its old
    # resolution and reporting it would describe a file that was never written.
    rendered_width = int(scene.render.resolution_x * scene.render.resolution_percentage / 100)
    rendered_height = int(scene.render.resolution_y * scene.render.resolution_percentage / 100)
    rendered_engine = scene.render.engine

    started = time.monotonic()
    produced: list[dict[str, Any]] = []
    try:
        for frame in frames:
            scene.frame_set(frame)
            path = _frame_path(output_path, frame, len(frames) > 1)
            scene.render.filepath = path
            bpy.ops.render.render(write_still=True)
            produced.append(
                {
                    "path": path,
                    "frame": frame,
                    "size_bytes": os.path.getsize(path) if os.path.exists(path) else 0,
                }
            )
    except RuntimeError as error:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            f"The render failed: {error}",
            {"engine": scene.render.engine, "frames": frames[:5]},
        ) from error
    finally:
        scene.render.filepath = previous_output
        scene.frame_set(previous_frame)
        scene.camera = previous_camera
        if restore:
            restore_settings(scene, previous)

    missing = [entry for entry in produced if entry["size_bytes"] == 0]
    if missing:
        raise BridgeError(
            ErrorCode.BLENDER_INTERNAL_ERROR,
            "Blender reported success but wrote no image. The output format or path may be "
            "unusable.",
            {"paths": [entry["path"] for entry in missing][:5]},
        )

    return {
        "files": produced,
        "engine": rendered_engine,
        "width": rendered_width,
        "height": rendered_height,
        "duration_ms": int((time.monotonic() - started) * 1000),
        "applied_settings": changed,
        "revision": ctx.revision,
    }


def _frames_to_render(scene, args: dict) -> list[int]:
    scope = c.optional(args, "scope")
    if scope is None or scope == "current" or (isinstance(scope, dict) and "current" in scope):
        return [scene.frame_current]
    if isinstance(scope, dict) and "frame" in scope:
        return [int(scope["frame"])]
    if isinstance(scope, dict) and "range" in scope:
        span = scope["range"]
        start = int(span["start"])
        end = int(span["end"])
        step = max(1, int(span.get("step", 1)))
        return list(range(start, end + 1, step))
    raise invalid_argument(
        "`scope` must be `current`, {\"frame\": n} or {\"range\": {...}}.", field="scope"
    )


def _frame_path(base: str, frame: int, numbered: bool) -> str:
    if not numbered:
        return base
    stem, extension = os.path.splitext(base)
    return f"{stem}_{frame:04d}{extension}"


@external("render.viewport_screenshot")
def viewport_screenshot(ctx, args: dict) -> dict[str, Any]:
    """Capture the viewport, or the camera view, without a full render.

    Uses the GPU offscreen API rather than `bpy.ops.screen.screenshot`, which
    needs a real window. That still rules it out in background mode, and the
    error says so rather than producing a blank image.
    """
    if bpy.app.background:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            "Viewport capture needs a 3D viewport, and this Blender is running in background "
            "mode. Use `render.execute` instead, which works headless.",
            {"background": True},
        )

    output_path = c.require_str(args, "output_path")
    width = c.optional_int(args, "width", 1920) or 1920
    height = c.optional_int(args, "height", 1080) or 1080
    shading = c.optional_str(args, "shading", "SOLID") or "SOLID"
    camera_view = c.optional_bool(args, "camera_view", False)

    import gpu
    from gpu_extras.presets import draw_texture_2d  # noqa: F401  (import validates gpu module)

    scene = bpy.context.scene
    view_layer = bpy.context.view_layer

    area = next((a for a in bpy.context.screen.areas if a.type == "VIEW_3D"), None)
    if area is None:
        raise BridgeError(
            ErrorCode.BLENDER_CONTEXT_ERROR,
            "No 3D viewport is open to capture.",
            {},
        )
    space = area.spaces.active
    region = next((r for r in area.regions if r.type == "WINDOW"), None)

    # `draw_view3d` renders with whatever the space is currently set to, so the
    # requested shading has to be applied to the space and put back afterwards.
    # Which modes exist depends on the build, so ask rather than assume.
    supported = [item.identifier for item in space.shading.bl_rna.properties["type"].enum_items]
    if shading not in supported:
        raise invalid_enum("shading", shading, supported)
    previous_shading = space.shading.type
    space.shading.type = shading

    offscreen = gpu.types.GPUOffScreen(width, height)
    started = time.monotonic()
    try:
        if camera_view:
            camera = camera_arg(args)
            previous_camera = scene.camera
            scene.camera = camera
            try:
                view_matrix = camera.matrix_world.inverted()
                projection = camera.calc_matrix_camera(
                    bpy.context.evaluated_depsgraph_get(), x=width, y=height
                )
                offscreen.draw_view3d(
                    scene,
                    view_layer,
                    space,
                    region,
                    view_matrix,
                    projection,
                    do_color_management=True,
                )
            finally:
                scene.camera = previous_camera
        else:
            offscreen.draw_view3d(
                scene,
                view_layer,
                space,
                region,
                region.data.view_matrix if hasattr(region, "data") else space.region_3d.view_matrix,
                space.region_3d.window_matrix,
                do_color_management=True,
            )

        buffer = offscreen.texture_color.read()
        buffer.dimensions = width * height * 4
    finally:
        offscreen.free()
        space.shading.type = previous_shading

    image = bpy.data.images.new("mcp_screenshot", width, height, alpha=True)
    try:
        image.pixels.foreach_set([value / 255.0 for value in buffer])
        image.filepath_raw = output_path
        image.file_format = "PNG"
        image.save()
        size = os.path.getsize(output_path) if os.path.exists(output_path) else 0
    finally:
        bpy.data.images.remove(image)

    return {
        "files": [{"path": output_path, "frame": scene.frame_current, "size_bytes": size}],
        "width": width,
        "height": height,
        "shading": shading,
        "duration_ms": int((time.monotonic() - started) * 1000),
        "revision": ctx.revision,
    }

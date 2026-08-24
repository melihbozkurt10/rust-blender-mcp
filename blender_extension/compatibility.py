"""Blender-version differences, in one place.

Blender renames things between releases: ``BLENDER_EEVEE`` became
``BLENDER_EEVEE_NEXT`` in 4.2, the Principled BSDF's sockets were reorganised
in 4.0, importers moved from ``bpy.ops.import_scene`` to ``bpy.ops.wm``, and
node group interfaces moved from ``tree.inputs`` to ``tree.interface``.

Rather than scattering version checks through the operation modules, everything
is introspected once here and reported to the server as capabilities. The
server then validates against what this build actually has, so an operation is
either supported or refused with a clear reason -- never silently wrong.
"""

from __future__ import annotations

import sys
from typing import Any

import bpy

from .protocol import BridgeError, ErrorCode


def blender_version() -> tuple[int, int, int]:
    return tuple(bpy.app.version)  # type: ignore[return-value]


def at_least(major: int, minor: int = 0, patch: int = 0) -> bool:
    return blender_version() >= (major, minor, patch)


def identity() -> dict[str, Any]:
    """Who we are, for the handshake."""
    from . import config

    major, minor, patch = blender_version()
    return {
        "blender_version": {"major": major, "minor": minor, "patch": patch},
        "python_version": ".".join(str(p) for p in sys.version_info[:3]),
        "addon_version": config.ADDON_VERSION,
        "platform": _platform(),
        "background": bool(bpy.app.background),
    }


def _platform() -> str:
    if sys.platform.startswith("win"):
        return "windows"
    if sys.platform.startswith("darwin"):
        return "darwin"
    return "linux"


# --- render engines ---------------------------------------------------------


def available_render_engines() -> list[str]:
    """Engine identifiers this build accepts for ``scene.render.engine``."""
    try:
        prop = bpy.types.RenderSettings.bl_rna.properties["engine"]
        return [item.identifier for item in prop.enum_items]
    except (KeyError, AttributeError):
        # Extremely defensive: if RNA introspection ever changes shape, report
        # nothing rather than crash, and the server falls back to trusting
        # Blender.
        return []


#: Preference order for the semantic engine names the protocol exposes.
ENGINE_CANDIDATES = {
    "CYCLES": ("CYCLES",),
    "EEVEE": ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"),
    "WORKBENCH": ("BLENDER_WORKBENCH",),
}


def resolve_engine(name: str) -> str:
    """Map a semantic engine name onto what this build calls it."""
    available = set(available_render_engines())
    if name in available:
        return name
    for candidate in ENGINE_CANDIDATES.get(name.upper(), ()):
        if candidate in available:
            return candidate
    raise BridgeError(
        ErrorCode.CAPABILITY_UNAVAILABLE,
        f"`{name}` is not an available render engine in this Blender build.",
        {"requested": name, "available": sorted(available)},
    )


# --- modifiers, constraints, nodes -----------------------------------------


def available_modifiers() -> list[str]:
    try:
        prop = bpy.types.Modifier.bl_rna.properties["type"]
        return [item.identifier for item in prop.enum_items]
    except (KeyError, AttributeError):
        return []


def available_constraints() -> list[str]:
    try:
        prop = bpy.types.Constraint.bl_rna.properties["type"]
        return [item.identifier for item in prop.enum_items]
    except (KeyError, AttributeError):
        return []


def _node_idnames(base) -> list[str]:
    """Every registered node type derived from a base class.

    ``__subclasses__`` does not work here: Blender creates the Python wrapper
    for an RNA type lazily, so a class nobody has touched yet is not a
    subclass of anything as far as Python is concerned. Walking ``bpy.types``
    forces each wrapper into existence, which is why this is done once at
    handshake time rather than per request.
    """
    found: set[str] = set()
    for name in dir(bpy.types):
        candidate = getattr(bpy.types, name, None)
        if not isinstance(candidate, type) or candidate is base:
            continue
        try:
            if not issubclass(candidate, base):
                continue
        except TypeError:
            continue
        rna = getattr(candidate, "bl_rna", None)
        identifier = getattr(rna, "identifier", None) or name
        found.add(identifier)
    return sorted(found)


def available_shader_nodes() -> list[str]:
    try:
        return _node_idnames(bpy.types.ShaderNode) + ["NodeGroupInput", "NodeGroupOutput", "ShaderNodeGroup"]
    except AttributeError:
        return []


def available_geometry_nodes() -> list[str]:
    try:
        return _node_idnames(bpy.types.GeometryNode) + [
            "NodeGroupInput",
            "NodeGroupOutput",
            "GeometryNodeGroup",
            # Function nodes are usable in geometry trees and are registered
            # under a separate base class.
            *_node_idnames(bpy.types.FunctionNode),
        ]
    except AttributeError:
        return []


def available_bake_types() -> list[str]:
    try:
        prop = bpy.types.BakeSettings.bl_rna.properties["cage_object"]  # probe RNA presence
        _ = prop
    except (KeyError, AttributeError):
        return []
    try:
        prop = bpy.types.Scene.bl_rna.properties["cycles"]
        _ = prop
    except (KeyError, AttributeError):
        # Cycles is not enabled, so nothing can be baked.
        return []
    return [
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


def available_image_formats() -> list[str]:
    try:
        prop = bpy.types.ImageFormatSettings.bl_rna.properties["file_format"]
        return [item.identifier for item in prop.enum_items]
    except (KeyError, AttributeError):
        return []


# --- import / export --------------------------------------------------------

#: format -> (import operator path, export operator path). Several are tried in
#: order because the built-in importers moved between releases.
IO_OPERATORS: dict[str, dict[str, tuple[str, ...]]] = {
    "FBX": {
        "import": ("import_scene.fbx",),
        "export": ("export_scene.fbx",),
    },
    "OBJ": {
        # 4.x ships the C++ importer as wm.obj_import; the old Python one was
        # import_scene.obj.
        "import": ("wm.obj_import", "import_scene.obj"),
        "export": ("wm.obj_export", "export_scene.obj"),
    },
    "GLTF": {
        "import": ("import_scene.gltf",),
        "export": ("export_scene.gltf",),
    },
    "GLB": {
        "import": ("import_scene.gltf",),
        "export": ("export_scene.gltf",),
    },
    "USD": {
        "import": ("wm.usd_import",),
        "export": ("wm.usd_export",),
    },
    "USDZ": {
        "import": ("wm.usd_import",),
        "export": ("wm.usd_export",),
    },
    "STL": {
        "import": ("wm.stl_import", "import_mesh.stl"),
        "export": ("wm.stl_export", "export_mesh.stl"),
    },
    "DAE": {
        "import": ("wm.collada_import",),
        "export": ("wm.collada_export",),
    },
    "PLY": {
        "import": ("wm.ply_import", "import_mesh.ply"),
        "export": ("wm.ply_export", "export_mesh.ply"),
    },
    "SVG": {
        "import": ("import_curve.svg",),
        "export": (),
    },
    "ABC": {
        "import": ("wm.alembic_import",),
        "export": ("wm.alembic_export",),
    },
    "BLEND": {
        "import": ("wm.append",),
        "export": ("wm.save_as_mainfile",),
    },
}


def _operator_exists(path: str) -> bool:
    module, _, name = path.partition(".")
    try:
        category = getattr(bpy.ops, module)
        return hasattr(category, name)
    except AttributeError:
        return False


def resolve_io_operator(fmt: str, direction: str) -> str:
    """The operator this build uses for a format, or raise."""
    candidates = IO_OPERATORS.get(fmt.upper(), {}).get(direction, ())
    for path in candidates:
        if _operator_exists(path):
            return path
    raise BridgeError(
        ErrorCode.UNSUPPORTED_FORMAT,
        f"This Blender build has no {direction}er for {fmt}. "
        "The matching add-on may be disabled in Preferences.",
        {"format": fmt, "direction": direction, "tried": list(candidates)},
    )


def available_io_formats(direction: str) -> list[str]:
    found = []
    for fmt, directions in IO_OPERATORS.items():
        for path in directions.get(direction, ()):
            if _operator_exists(path):
                found.append(fmt)
                break
    return sorted(found)


# --- node tree interfaces ---------------------------------------------------


def uses_tree_interface() -> bool:
    """Whether node groups expose the 4.x ``interface`` API."""
    return hasattr(bpy.types.NodeTree, "interface") or at_least(4, 0)


# --- Principled BSDF socket names ------------------------------------------

#: Semantic name -> candidate socket identifiers, newest first. Blender 4.0
#: renamed most of these, and several still differ in 4.2+.
PRINCIPLED_SOCKETS: dict[str, tuple[str, ...]] = {
    "base_color": ("Base Color",),
    "metallic": ("Metallic",),
    "roughness": ("Roughness",),
    "ior": ("IOR",),
    "alpha": ("Alpha",),
    "emission_color": ("Emission Color", "Emission"),
    "emission_strength": ("Emission Strength",),
    "specular": ("Specular IOR Level", "Specular"),
    "transmission": ("Transmission Weight", "Transmission"),
    "coat_weight": ("Coat Weight", "Clearcoat"),
    "coat_roughness": ("Coat Roughness", "Clearcoat Roughness"),
    "sheen_weight": ("Sheen Weight", "Sheen"),
    "sheen_roughness": ("Sheen Roughness", "Sheen Tint"),
    "anisotropic": ("Anisotropic",),
    "anisotropic_rotation": ("Anisotropic Rotation",),
    "normal": ("Normal",),
}


def principled_socket(node, semantic: str):
    """The socket on a Principled BSDF for a semantic name, or ``None``."""
    for candidate in PRINCIPLED_SOCKETS.get(semantic, ()):
        socket = node.inputs.get(candidate)
        if socket is not None:
            return socket
    return None


def require_principled_socket(node, semantic: str):
    socket = principled_socket(node, semantic)
    if socket is None:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_PROPERTY,
            f"This Blender build's Principled BSDF has no `{semantic}` input.",
            {
                "requested": semantic,
                "tried": list(PRINCIPLED_SOCKETS.get(semantic, ())),
                "available": [s.name for s in node.inputs],
            },
        )
    return socket


# --- capability report ------------------------------------------------------


def capabilities() -> dict[str, Any]:
    """Everything the server needs to validate requests against this build."""
    engines = available_render_engines()
    return {
        "render_engines": engines,
        "modifiers": available_modifiers(),
        "shader_nodes": available_shader_nodes(),
        "geometry_nodes": available_geometry_nodes(),
        "constraints": available_constraints(),
        "bone_constraints": available_constraints(),
        "bake_types": available_bake_types(),
        "image_formats": available_image_formats(),
        "import_formats": available_io_formats("import"),
        "export_formats": available_io_formats("export"),
        "features": {
            "cycles": "CYCLES" in engines,
            "eevee": any(e.startswith("BLENDER_EEVEE") for e in engines),
            "workbench": "BLENDER_WORKBENCH" in engines,
            "geometry_nodes": hasattr(bpy.types, "GeometryNode"),
            "shader_nodes": hasattr(bpy.types, "ShaderNode"),
            "compositor": hasattr(bpy.types, "CompositorNode"),
            "gpu_offscreen_render": not bpy.app.background,
            "undo_stack": hasattr(bpy.ops.ed, "undo_push"),
            "node_tree_interface": uses_tree_interface(),
        },
    }


# --- actions: layered vs legacy ---------------------------------------------
#
# Blender 4.4 replaced the flat ``Action.fcurves`` list with slotted, layered
# actions: an action holds layers, each layer holds strips, and a keyframe
# strip holds one channelbag per slot, each with its own F-curves. The old
# attribute is gone in 5.x. Every F-curve access in the bridge goes through the
# helpers below so both shapes work from one code path.


def action_is_layered(action) -> bool:
    return not hasattr(action, "fcurves")


def slot_handle_for(owner) -> int | None:
    """The action slot a data-block is bound to, if the build has slots."""
    animation_data = getattr(owner, "animation_data", None)
    if animation_data is None:
        return None
    slot = getattr(animation_data, "action_slot", None)
    if slot is None:
        return None
    return getattr(slot, "handle", None)


def iter_channelbags(action, slot_handle: int | None = None):
    """Every channelbag in a layered action, optionally for one slot."""
    for layer in getattr(action, "layers", []):
        for strip in layer.strips:
            for bag in getattr(strip, "channelbags", []):
                if slot_handle is not None and getattr(bag, "slot_handle", None) != slot_handle:
                    continue
                yield bag


def action_fcurves(action, slot_handle: int | None = None) -> list:
    """Every F-curve in an action, whichever shape the build uses."""
    if not action_is_layered(action):
        return list(action.fcurves)
    curves = []
    for bag in iter_channelbags(action, slot_handle):
        curves.extend(bag.fcurves)
    return curves


def fcurve_containers(action, slot_handle: int | None = None):
    """`(container, fcurve)` pairs, so a curve can be removed from its owner."""
    if not action_is_layered(action):
        for curve in list(action.fcurves):
            yield action.fcurves, curve
        return
    for bag in iter_channelbags(action, slot_handle):
        for curve in list(bag.fcurves):
            yield bag.fcurves, curve


def owner_fcurves(owner) -> list:
    """F-curves currently driving a data-block."""
    animation_data = getattr(owner, "animation_data", None)
    if animation_data is None or animation_data.action is None:
        return []
    return action_fcurves(animation_data.action, slot_handle_for(owner))


def owner_fcurve_containers(owner):
    animation_data = getattr(owner, "animation_data", None)
    if animation_data is None or animation_data.action is None:
        return iter(())
    return fcurve_containers(animation_data.action, slot_handle_for(owner))


def action_frame_range(action) -> tuple[float, float]:
    """An action frame range, computed from its curves when it reports none."""
    reported = tuple(action.frame_range)
    if reported[0] != reported[1]:
        return (float(reported[0]), float(reported[1]))
    frames = [
        point.co[0] for curve in action_fcurves(action) for point in curve.keyframe_points
    ]
    if not frames:
        return (float(reported[0]), float(reported[1]))
    return (float(min(frames)), float(max(frames)))

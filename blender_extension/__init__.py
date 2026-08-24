"""Blender MCP Bridge.

A thin, persistent connection between Blender and the Rust MCP server. This
add-on decodes frames, dispatches them through a fixed table of handlers, and
calls ``bpy``. It contains no MCP implementation, no policy, and -- deliberately
-- no way to execute code that arrived over the network.

Installs both ways:

* as an extension (Blender 4.2+), via ``blender_manifest.toml``;
* as a legacy add-on, via the ``bl_info`` below.
"""

from __future__ import annotations

bl_info = {
    "name": "Blender MCP Bridge",
    "author": "blender-mcp contributors",
    "version": (0, 1, 0),
    "blender": (4, 2, 0),
    "location": "View3D > Sidebar > MCP",
    "description": "Typed, persistent bridge to the Rust Blender MCP server",
    "category": "System",
}


def register() -> None:
    from . import addon

    addon.register()


def unregister() -> None:
    from . import addon

    addon.unregister()

"""Add-on preferences: where to connect, and whether to do it automatically."""

from __future__ import annotations

import bpy

from . import config


class BlenderMcpPreferences(bpy.types.AddonPreferences):
    """Preferences for the Blender MCP bridge."""

    # Set at registration time, because the key must match the package name and
    # that differs between a legacy add-on and an extension.
    bl_idname = __package__ or "blender_extension"

    host: bpy.props.StringProperty(  # type: ignore[valid-type]
        name="Host",
        description="Address the MCP server listens on. Loopback unless you know why not",
        default=config.DEFAULT_HOST,
    )
    port: bpy.props.IntProperty(  # type: ignore[valid-type]
        name="Port",
        description="Port the MCP server listens on",
        default=config.DEFAULT_PORT,
        min=1,
        max=65535,
    )
    auto_connect: bpy.props.BoolProperty(  # type: ignore[valid-type]
        name="Connect on startup",
        description="Start connecting as soon as the add-on is enabled",
        default=True,
    )
    auto_reconnect: bpy.props.BoolProperty(  # type: ignore[valid-type]
        name="Reconnect automatically",
        description="Keep retrying when the MCP server is not running yet, or restarts",
        default=True,
    )

    def draw(self, context: bpy.types.Context) -> None:
        layout = self.layout
        column = layout.column()
        row = column.row(align=True)
        row.prop(self, "host")
        row.prop(self, "port")
        column.prop(self, "auto_connect")
        column.prop(self, "auto_reconnect")

        if self.host not in {"127.0.0.1", "localhost", "::1"}:
            warning = column.box()
            warning.alert = True
            warning.label(text="Non-loopback host: any machine that can reach", icon="ERROR")
            warning.label(text="this address can edit your scene.")


def get(context: bpy.types.Context | None = None) -> BlenderMcpPreferences | None:
    """The add-on's preferences, or ``None`` if it is not registered."""
    context = context or bpy.context
    addon = context.preferences.addons.get(BlenderMcpPreferences.bl_idname)
    return addon.preferences if addon is not None else None


def host_and_port(context: bpy.types.Context | None = None) -> tuple[str, int]:
    prefs = get(context)
    if prefs is None:
        return config.DEFAULT_HOST, config.DEFAULT_PORT
    return prefs.host, prefs.port


CLASSES = (BlenderMcpPreferences,)

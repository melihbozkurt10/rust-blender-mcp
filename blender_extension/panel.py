"""The N-panel: connection state, and the three buttons that change it."""

from __future__ import annotations

import bpy

from . import preferences, transport
from .dispatcher import STATE


class BLENDERMCP_OT_connect(bpy.types.Operator):
    bl_idname = "blendermcp.connect"
    bl_label = "Connect"
    bl_description = "Start connecting to the MCP server"

    def execute(self, context):
        prefs = preferences.get(context)
        host, port = preferences.host_and_port(context)
        auto_reconnect = prefs.auto_reconnect if prefs is not None else True
        transport.BRIDGE.start(host, port, auto_reconnect=auto_reconnect)
        self.report({"INFO"}, f"Connecting to {host}:{port}")
        return {"FINISHED"}


class BLENDERMCP_OT_disconnect(bpy.types.Operator):
    bl_idname = "blendermcp.disconnect"
    bl_label = "Disconnect"
    bl_description = "Stop the bridge and close the connection"

    def execute(self, context):
        transport.BRIDGE.stop()
        self.report({"INFO"}, "Disconnected")
        return {"FINISHED"}


class BLENDERMCP_OT_reconnect(bpy.types.Operator):
    bl_idname = "blendermcp.reconnect"
    bl_label = "Reconnect"
    bl_description = "Drop the current connection and dial again"

    def execute(self, context):
        transport.BRIDGE.reconnect()
        self.report({"INFO"}, "Reconnecting")
        return {"FINISHED"}


class BLENDERMCP_OT_copy_diagnostics(bpy.types.Operator):
    bl_idname = "blendermcp.copy_diagnostics"
    bl_label = "Copy Diagnostics"
    bl_description = "Copy connection details to the clipboard for a bug report"

    def execute(self, context):
        import json

        from . import compatibility

        report = {
            "status": transport.status(),
            "identity": compatibility.identity(),
            "capability_counts": {
                key: len(value)
                for key, value in compatibility.capabilities().items()
                if isinstance(value, list)
            },
        }
        context.window_manager.clipboard = json.dumps(report, indent=2)
        self.report({"INFO"}, "Diagnostics copied to the clipboard")
        return {"FINISHED"}


class BLENDERMCP_PT_panel(bpy.types.Panel):
    bl_label = "Blender MCP"
    bl_idname = "BLENDERMCP_PT_panel"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = "MCP"

    def draw(self, context):
        layout = self.layout
        status = transport.status()

        box = layout.box()
        if status["connected"]:
            box.label(text="Connected", icon="LINKED")
        elif status["running"]:
            box.label(text="Waiting for the server", icon="SORTTIME")
        else:
            box.label(text="Not running", icon="UNLINKED")

        column = box.column(align=True)
        column.label(text=f"Server: {status['address']}")
        column.label(text=f"Protocol: v{status['protocol_version']}")
        column.label(text=f"Add-on: {status['addon_version']}")
        column.label(text=f"Blender: {bpy.app.version_string}")
        column.label(text=f"Revision: {status['revision']}")

        stats = status["stats"]
        column.label(text=f"Requests: {stats['requests']}  Errors: {stats['errors']}")
        if status["queued_in"] or status["queued_out"]:
            column.label(text=f"Queued in/out: {status['queued_in']}/{status['queued_out']}")

        if status["last_error"]:
            error_box = layout.box()
            error_box.alert = True
            error_box.label(text="Last error:", icon="ERROR")
            for line in _wrap(status["last_error"], 40):
                error_box.label(text=line)

        row = layout.row(align=True)
        if status["running"]:
            row.operator(BLENDERMCP_OT_disconnect.bl_idname, icon="CANCEL")
            row.operator(BLENDERMCP_OT_reconnect.bl_idname, icon="FILE_REFRESH")
        else:
            row.operator(BLENDERMCP_OT_connect.bl_idname, icon="PLAY")
        layout.operator(BLENDERMCP_OT_copy_diagnostics.bl_idname, icon="COPYDOWN")


def _wrap(text: str, width: int) -> list[str]:
    """Blender labels do not wrap, so long errors are split by hand."""
    words = text.split()
    lines: list[str] = []
    current = ""
    for word in words:
        candidate = f"{current} {word}".strip()
        if len(candidate) > width and current:
            lines.append(current)
            current = word
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines[:6]


CLASSES = (
    BLENDERMCP_OT_connect,
    BLENDERMCP_OT_disconnect,
    BLENDERMCP_OT_reconnect,
    BLENDERMCP_OT_copy_diagnostics,
    BLENDERMCP_PT_panel,
)

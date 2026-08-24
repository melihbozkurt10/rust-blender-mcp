"""Add-on lifecycle.

Enabling and disabling repeatedly during development is the normal case, not
the exception, so unregister has to be exact: every timer removed, every
handler removed, every thread joined. A leaked socket thread keeps the port and
the next enable fails in a way that looks like a server bug.
"""

from __future__ import annotations

import bpy

from . import config, dispatcher, events, panel, preferences, transport

# Importing the operations package is what registers the handlers.
from . import operations  # noqa: F401  (side effects)


def _register_classes() -> None:
    for cls in (*preferences.CLASSES, *panel.CLASSES):
        try:
            bpy.utils.register_class(cls)
        except ValueError:
            # Already registered by a half-completed previous enable.
            bpy.utils.unregister_class(cls)
            bpy.utils.register_class(cls)


def _unregister_classes() -> None:
    for cls in reversed((*preferences.CLASSES, *panel.CLASSES)):
        try:
            bpy.utils.unregister_class(cls)
        except RuntimeError:
            # Not registered. Nothing to undo.
            pass


def _start_timer() -> None:
    if not bpy.app.timers.is_registered(dispatcher.pump):
        bpy.app.timers.register(dispatcher.pump, persistent=True)


def _stop_timer() -> None:
    if bpy.app.timers.is_registered(dispatcher.pump):
        bpy.app.timers.unregister(dispatcher.pump)


def register() -> None:
    _register_classes()
    events.register()
    _start_timer()

    print(
        f"[blender-mcp] add-on {config.ADDON_VERSION} registered with "
        f"{operations.operation_count()} operations"
    )

    prefs = preferences.get()
    if prefs is not None and prefs.auto_connect:
        transport.BRIDGE.start(prefs.host, prefs.port, auto_reconnect=prefs.auto_reconnect)


def unregister() -> None:
    transport.BRIDGE.stop()
    _stop_timer()
    events.unregister()
    _unregister_classes()
    print("[blender-mcp] add-on unregistered")


def start_headless(host: str | None = None, port: int | None = None) -> None:
    """Connect without any UI. Used by the headless smoke tests.

    ``bpy.app.timers`` still runs in background mode as long as something keeps
    the main loop alive, which the test harness does explicitly.
    """
    _start_timer()
    events.register()
    transport.BRIDGE.start(
        host or config.DEFAULT_HOST,
        port or config.DEFAULT_PORT,
        auto_reconnect=True,
    )

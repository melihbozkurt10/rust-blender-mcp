"""Watching what the user does in Blender.

The person at the keyboard is editing the same scene as the model. Without
these notifications the server's cache drifts from reality within seconds of a
human touching anything.

The depsgraph gives precise per-datablock update flags, so transform, geometry
and shading changes are reported as what they are. Creation, deletion and
renaming are not in the depsgraph's vocabulary, so those are detected by
diffing a small snapshot -- ids and names only, never geometry.
"""

from __future__ import annotations

from typing import Any

import bpy
from bpy.app.handlers import persistent

from . import ids
from .dispatcher import STATE

#: Last seen {object id: name}, for create/delete/rename detection.
_OBJECT_SNAPSHOT: dict[str, str] = {}
#: Last seen selection, as ids.
_SELECTION: set[str] = set()
_ACTIVE: str | None = None
_SCENE: str | None = None

#: Above this many objects, the per-update diff is skipped and a coarse
#: invalidation is sent instead. Diffing a 50 000-object scene on every
#: depsgraph tick would cost more than the cache saves.
SNAPSHOT_LIMIT = 5000


def _snapshot_objects() -> dict[str, str]:
    return {ids.ensure_id(obj): obj.name for obj in bpy.data.objects}


def reset() -> None:
    """Re-baseline. Called on connect and after loading a file."""
    global _ACTIVE, _SCENE
    _OBJECT_SNAPSHOT.clear()
    _SELECTION.clear()
    _ACTIVE = None
    _SCENE = None
    if len(bpy.data.objects) <= SNAPSHOT_LIMIT:
        _OBJECT_SNAPSHOT.update(_snapshot_objects())
    scene = bpy.context.scene if bpy.context else None
    if scene is not None:
        _SCENE = scene.name
        _capture_selection()


def _capture_selection() -> tuple[set[str], str | None]:
    view_layer = getattr(bpy.context, "view_layer", None)
    if view_layer is None:
        return set(), None
    # During `load_post` on a freshly emptied file the view layer can still hand
    # out a null entry; touching it raises and kills the handler.
    selected = {
        ids.ensure_id(obj)
        for obj in view_layer.objects
        if obj is not None and obj.select_get()
    }
    active = view_layer.objects.active
    return selected, ids.ensure_id(active) if active is not None else None


@persistent
def on_depsgraph_update(scene, depsgraph) -> None:
    """Report what changed since the last tick."""
    if not STATE.connected or STATE.session_id is None:
        return
    try:
        _report_structural_changes()
        _report_datablock_updates(depsgraph)
        _report_selection()
    except Exception as error:  # noqa: BLE001 - a handler that raises is unregistered by Blender
        print(f"[blender-mcp] event handler error: {type(error).__name__}: {error}")


def _report_structural_changes() -> None:
    global _OBJECT_SNAPSHOT

    if len(bpy.data.objects) > SNAPSHOT_LIMIT:
        # Too big to diff cheaply. Tell the server its object cache is suspect
        # and let it re-read on demand.
        if _OBJECT_SNAPSHOT:
            _OBJECT_SNAPSHOT = {}
            STATE.bump_revision()
            STATE.emit_event("file_reloaded")
        return

    current = _snapshot_objects()
    if current == _OBJECT_SNAPSHOT:
        return

    previous = _OBJECT_SNAPSHOT
    for entity_id, name in current.items():
        old_name = previous.get(entity_id)
        if old_name is None:
            STATE.bump_revision()
            STATE.emit_event("created", kind="object", id=entity_id, name=name)
        elif old_name != name:
            STATE.bump_revision()
            STATE.emit_event("renamed", kind="object", id=entity_id, **{"from": old_name, "to": name})

    for entity_id, name in previous.items():
        if entity_id not in current:
            STATE.bump_revision()
            STATE.emit_event("deleted", kind="object", id=entity_id, name=name)

    _OBJECT_SNAPSHOT = current


def _report_datablock_updates(depsgraph) -> None:
    for update in depsgraph.updates:
        datablock = update.id
        if not isinstance(datablock, bpy.types.Object):
            _report_non_object_update(update, datablock)
            continue

        # `update.id` is the evaluated copy; the original carries the id.
        original = datablock.original
        entity_id = ids.peek_id(original)
        if entity_id is None:
            continue

        fields: list[str] = []
        if update.is_updated_transform:
            fields.append("transform")
        if update.is_updated_shading:
            fields.append("materials")
        if update.is_updated_geometry:
            # Geometry changes invalidate every cached index, so this gets its
            # own event rather than being folded into a field list.
            mesh = original.data
            revision = ids.next_mesh_revision(mesh) if _is_mesh(mesh) else 0
            STATE.bump_revision()
            STATE.emit_event("mesh_invalidated", object_id=entity_id, mesh_revision=revision)

        if fields:
            STATE.bump_revision()
            STATE.emit_event("modified", kind="object", id=entity_id, fields=fields)


def _is_mesh(datablock: Any) -> bool:
    return isinstance(datablock, bpy.types.Mesh)


def _report_non_object_update(update, datablock) -> None:
    """Coarse invalidation for the data-blocks worth tracking."""
    if isinstance(datablock, bpy.types.Material):
        entity_id = ids.peek_id(datablock.original)
        if entity_id is not None and update.is_updated_shading:
            STATE.bump_revision()
            STATE.emit_event("node_tree_invalidated", node_tree_id=entity_id)
    elif isinstance(datablock, bpy.types.NodeTree):
        entity_id = ids.peek_id(datablock.original)
        if entity_id is not None:
            STATE.bump_revision()
            STATE.emit_event("node_tree_invalidated", node_tree_id=entity_id)


def _report_selection() -> None:
    global _SELECTION, _ACTIVE

    selected, active = _capture_selection()
    if selected == _SELECTION and active == _ACTIVE:
        return
    _SELECTION = selected
    _ACTIVE = active
    STATE.bump_revision()
    STATE.emit_event("selection_changed", selected=sorted(selected), active=active)


@persistent
def on_load_post(_dummy) -> None:
    """A new file was opened, or the current one reverted."""
    ids.invalidate_cache()
    reset()
    if STATE.connected:
        STATE.bump_revision()
        STATE.emit_event("file_reloaded", filepath=bpy.data.filepath or None)


@persistent
def on_undo_redo(_scene) -> None:
    """Undo and redo can change anything, including ids."""
    ids.invalidate_cache()
    reset()
    if STATE.connected:
        STATE.bump_revision()
        STATE.emit_event("file_reloaded", filepath=bpy.data.filepath or None)


_HANDLERS = (
    (bpy.app.handlers.depsgraph_update_post, on_depsgraph_update),
    (bpy.app.handlers.load_post, on_load_post),
    (bpy.app.handlers.undo_post, on_undo_redo),
    (bpy.app.handlers.redo_post, on_undo_redo),
)


def register() -> None:
    for collection, handler in _HANDLERS:
        if handler not in collection:
            collection.append(handler)


def unregister() -> None:
    for collection, handler in _HANDLERS:
        # Repeated enable/disable cycles during development leave duplicates
        # behind unless every copy is removed.
        while handler in collection:
            collection.remove(handler)

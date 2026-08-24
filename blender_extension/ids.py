"""Stable entity identity.

Blender names are mutable and only unique within a data-block collection, so
they cannot be the identifier the server holds onto. Every data-block the
bridge touches gets an ``mcp_id`` custom property holding a UUID, which
survives renames, saves and reloads.

Lookup goes id -> data-block through a cache that is rebuilt whenever an id is
missed, because a scan of ``bpy.data`` is cheap next to the cost of getting the
answer wrong.
"""

from __future__ import annotations

import uuid
from typing import Any, Iterable

import bpy

from . import config
from .protocol import BridgeError, ErrorCode, not_found

#: Which ``bpy.data`` collection holds each entity kind.
COLLECTION_BY_KIND = {
    "object": "objects",
    "mesh": "meshes",
    "material": "materials",
    "collection": "collections",
    "node_tree": "node_groups",
    "action": "actions",
    "armature": "armatures",
    "camera": "cameras",
    "light": "lights",
    "image": "images",
    "texture": "textures",
    "scene": "scenes",
    "world": "worlds",
}


def ensure_id(datablock: Any) -> str:
    """Return the data-block's stable id, assigning one if it has none."""
    if datablock is None:
        raise BridgeError(ErrorCode.INVALID_ARGUMENT, "Cannot assign an id to nothing.")
    existing = datablock.get(config.ID_PROPERTY)
    if isinstance(existing, str) and existing:
        return existing
    new_id = str(uuid.uuid4())
    try:
        datablock[config.ID_PROPERTY] = new_id
    except (TypeError, AttributeError) as exc:
        # Linked-in library data is read-only. Fall back to a deterministic id
        # derived from the library path and name, so at least the id is stable
        # within the session rather than failing the whole request.
        derived = uuid.uuid5(uuid.NAMESPACE_URL, f"blender-mcp:{_library_key(datablock)}")
        _READONLY_IDS[str(derived)] = datablock
        if not _is_linked(datablock):
            raise BridgeError(
                ErrorCode.BLENDER_INTERNAL_ERROR,
                f"Could not assign an id to `{getattr(datablock, 'name', '?')}`: {exc}",
            ) from exc
        return str(derived)
    return new_id


def peek_id(datablock: Any) -> str | None:
    """The data-block's id, without assigning one."""
    if datablock is None:
        return None
    value = datablock.get(config.ID_PROPERTY)
    return value if isinstance(value, str) and value else None


def _is_linked(datablock: Any) -> bool:
    return getattr(datablock, "library", None) is not None


def _library_key(datablock: Any) -> str:
    library = getattr(datablock, "library", None)
    prefix = getattr(library, "filepath", "") if library else ""
    return f"{prefix}/{type(datablock).__name__}/{getattr(datablock, 'name', '')}"


#: Ids handed out for read-only linked data, which cannot carry the property.
_READONLY_IDS: dict[str, Any] = {}

#: kind -> {id: name}
_CACHE: dict[str, dict[str, str]] = {}


def invalidate_cache(kind: str | None = None) -> None:
    """Drop the lookup cache, entirely or for one kind."""
    if kind is None:
        _CACHE.clear()
    else:
        _CACHE.pop(kind, None)


def _collection_for(kind: str):
    attribute = COLLECTION_BY_KIND.get(kind)
    if attribute is None:
        raise BridgeError(
            ErrorCode.INVALID_ARGUMENT,
            f"`{kind}` is not an addressable entity kind.",
            {"kind": kind, "known": sorted(COLLECTION_BY_KIND)},
        )
    return getattr(bpy.data, attribute)


def _rebuild(kind: str) -> dict[str, str]:
    index: dict[str, str] = {}
    for datablock in _collection_for(kind):
        value = peek_id(datablock)
        if value:
            index[value] = datablock.name
    _CACHE[kind] = index
    return index


def find(kind: str, reference: str, required: bool = True):
    """Resolve a reference, which is either a stable id or a current name.

    Ids win: a data-block whose *name* happens to look like a UUID cannot
    shadow a real id.
    """
    if not isinstance(reference, str) or not reference:
        raise BridgeError(
            ErrorCode.INVALID_ARGUMENT,
            f"A {kind} reference must be a non-empty string.",
            {"kind": kind},
        )

    collection = _collection_for(kind)

    if _looks_like_uuid(reference):
        index = _CACHE.get(kind) or _rebuild(kind)
        name = index.get(reference)
        datablock = collection.get(name) if name is not None else None
        # A stale cache entry (renamed or deleted since) is worth one rescan.
        if datablock is None or peek_id(datablock) != reference:
            index = _rebuild(kind)
            name = index.get(reference)
            datablock = collection.get(name) if name is not None else None
        if datablock is None:
            datablock = _READONLY_IDS.get(reference)
        if datablock is not None:
            return datablock
        if required:
            raise not_found(kind, reference)
        return None

    datablock = collection.get(reference)
    if datablock is None and required:
        candidates = [d.name for d in collection][:20]
        raise not_found(kind, reference, available=candidates)
    return datablock


def _looks_like_uuid(value: str) -> bool:
    try:
        uuid.UUID(value)
    except (ValueError, AttributeError, TypeError):
        return False
    return True


def find_object(reference: str, required: bool = True):
    return find("object", reference, required)


def find_material(reference: str, required: bool = True):
    return find("material", reference, required)


def find_collection(reference: str, required: bool = True):
    """Resolve a collection reference, treating the scene root specially."""
    if reference in {"Scene Collection", "__scene__"}:
        return bpy.context.scene.collection
    return find("collection", reference, required)


def resolve_all(kind: str, references: Iterable[str]) -> list[Any]:
    """Resolve a list of references, failing on the first that misses."""
    return [find(kind, reference) for reference in references]


def describe(datablock: Any) -> dict[str, Any] | None:
    """`{"id": ..., "name": ...}` for a data-block, or ``None``."""
    if datablock is None:
        return None
    return {"id": ensure_id(datablock), "name": datablock.name}


def next_mesh_revision(mesh: Any) -> int:
    """Bump and return a mesh's topology revision.

    Any operation that can add or remove elements calls this, so a caller
    holding vertex indices can tell they have gone stale.
    """
    current = mesh.get(config.MESH_REVISION_PROPERTY, 0)
    try:
        current = int(current)
    except (TypeError, ValueError):
        current = 0
    revision = current + 1
    mesh[config.MESH_REVISION_PROPERTY] = revision
    return revision


def mesh_revision(mesh: Any) -> int:
    """A mesh's current topology revision."""
    if mesh is None:
        return 0
    value = mesh.get(config.MESH_REVISION_PROPERTY, 0)
    try:
        return int(value)
    except (TypeError, ValueError):
        return 0


def check_mesh_revision(mesh: Any, expected: int | None) -> None:
    """Raise ``TOPOLOGY_STALE`` when the caller's indices predate an edit."""
    if expected is None:
        return
    actual = mesh_revision(mesh)
    if int(expected) != actual:
        raise BridgeError(
            ErrorCode.TOPOLOGY_STALE,
            "The mesh changed since these indices were read "
            f"(expected revision {expected}, mesh is at {actual}). Re-read the mesh and retry.",
            {"expected_mesh_revision": int(expected), "actual_mesh_revision": actual},
        )

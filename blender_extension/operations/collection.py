"""Collection operations."""

from __future__ import annotations

from typing import Any

import bpy

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

COLOR_TAGS = [
    "NONE",
    "COLOR_01",
    "COLOR_02",
    "COLOR_03",
    "COLOR_04",
    "COLOR_05",
    "COLOR_06",
    "COLOR_07",
    "COLOR_08",
]


def summarise(collection, *, detail: bool = False) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "id": ids.ensure_id(collection),
        "name": collection.name,
        "object_count": len(collection.objects),
        "child_count": len(collection.children),
        "color_tag": collection.color_tag,
        "hide_viewport": bool(collection.hide_viewport),
        "hide_render": bool(collection.hide_render),
        "exclude": _is_excluded(collection),
    }
    parent = _parent_of(collection)
    if parent is not None and parent != bpy.context.scene.collection:
        payload["parent"] = ids.ensure_id(parent)
    if detail:
        payload["objects"] = [obj.name for obj in collection.objects]
        payload["children"] = [child.name for child in collection.children]
    return payload


def _parent_of(collection):
    """The collection that contains this one, scene root included."""
    scene_root = bpy.context.scene.collection
    for candidate in [scene_root, *bpy.data.collections]:
        if collection.name in candidate.children:
            return candidate
    return None


def _layer_collection(collection, root=None):
    """The view-layer wrapper for a collection, which is where `exclude` lives."""
    root = root or bpy.context.view_layer.layer_collection
    if root.collection == collection:
        return root
    for child in root.children:
        found = _layer_collection(collection, child)
        if found is not None:
            return found
    return None


def _is_excluded(collection) -> bool:
    layer = _layer_collection(collection)
    return bool(layer.exclude) if layer is not None else False


@read("collection.list")
def list_collections(ctx, args: dict) -> dict[str, Any]:
    name_filter = c.optional_str(args, "name_contains")
    parent_reference = c.optional_str(args, "parent")
    recursive = c.optional_bool(args, "recursive", False)

    if parent_reference is not None:
        parent = ids.find_collection(parent_reference)
        candidates = _descendants(parent) if recursive else list(parent.children)
    else:
        candidates = list(bpy.data.collections)

    matched = [col for col in candidates if c.matches_name(col.name, name_filter)]
    matched.sort(key=lambda col: col.name)
    window, cursor = c.paginate(matched, args)
    return {
        "collections": [summarise(col) for col in window],
        "total": len(matched),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


def _descendants(collection) -> list:
    found = []
    for child in collection.children:
        found.append(child)
        found.extend(_descendants(child))
    return found


@read("collection.get")
def get(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    return {"collection": summarise(collection, detail=True), "revision": ctx.revision}


@op("collection.create")
def create(ctx, args: dict) -> dict[str, Any]:
    name = c.require_str(args, "name")
    parent = c.collection_arg(args, "parent") or bpy.context.scene.collection
    color_tag = c.optional_str(args, "color_tag")

    collection = bpy.data.collections.new(name)
    if color_tag is not None:
        collection.color_tag = c.enum_value(color_tag, COLOR_TAGS, "color_tag")
    parent.children.link(collection)

    for obj in c.objects_arg(args, "objects", required=False):
        _move_object(obj, collection, exclusive=True)

    ids.invalidate_cache("collection")
    ctx.bump()
    return {"collection": summarise(collection, detail=True), "revision": ctx.revision}


@op("collection.rename")
def rename(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    name = c.require_str(args, "name")
    previous = collection.name
    collection.name = name
    ids.invalidate_cache("collection")
    ctx.bump()
    return {
        "id": ids.ensure_id(collection),
        "from": previous,
        "to": collection.name,
        "revision": ctx.revision,
    }


@op("collection.delete")
def delete(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    delete_objects = c.optional_bool(args, "delete_objects", False)
    recursive = c.optional_bool(args, "recursive", False)

    if collection == bpy.context.scene.collection:
        raise invalid_argument("The scene's root collection cannot be deleted.")

    parent = _parent_of(collection) or bpy.context.scene.collection
    targets = [collection, *(_descendants(collection) if recursive else [])]

    removed_objects = []
    for target in targets:
        for obj in list(target.objects):
            if delete_objects:
                removed_objects.append({"id": ids.ensure_id(obj), "name": obj.name})
                bpy.data.objects.remove(obj, do_unlink=True)
            else:
                # Relinking rather than orphaning: an object that vanishes from
                # every collection is invisible but still in the file, which is
                # a confusing state to leave someone in.
                target.objects.unlink(obj)
                if not obj.users_collection:
                    parent.objects.link(obj)

    if not recursive:
        for child in list(collection.children):
            collection.children.unlink(child)
            parent.children.link(child)

    removed = []
    for target in reversed(targets):
        removed.append({"id": ids.ensure_id(target), "name": target.name})
        bpy.data.collections.remove(target)

    ids.invalidate_cache("collection")
    ids.invalidate_cache("object")
    ctx.bump()
    return {"deleted": removed, "deleted_objects": removed_objects, "revision": ctx.revision}


@op("collection.link_object")
def link_object(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    objects = c.objects_arg(args, "objects")
    for obj in objects:
        if obj.name not in collection.objects:
            collection.objects.link(obj)
    ctx.bump()
    return {"collection": summarise(collection), "revision": ctx.revision}


@op("collection.unlink_object")
def unlink_object(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    objects = c.objects_arg(args, "objects")
    for obj in objects:
        if obj.name not in collection.objects:
            continue
        if len(obj.users_collection) == 1:
            raise invalid_argument(
                f"`{obj.name}` is only in `{collection.name}`; unlinking it would leave it "
                "in no collection at all. Use `collection.move_object`, or delete the object.",
                object=obj.name,
            )
        collection.objects.unlink(obj)
    ctx.bump()
    return {"collection": summarise(collection), "revision": ctx.revision}


@op("collection.move_object")
def move_object(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    objects = c.objects_arg(args, "objects")
    exclusive = c.optional_bool(args, "exclusive", True)
    for obj in objects:
        _move_object(obj, collection, exclusive=exclusive)
    ctx.bump()
    return {"collection": summarise(collection, detail=True), "revision": ctx.revision}


def _move_object(obj, collection, exclusive: bool) -> None:
    if exclusive:
        for existing in list(obj.users_collection):
            if existing != collection:
                existing.objects.unlink(obj)
    if obj.name not in collection.objects:
        collection.objects.link(obj)


@op("collection.set_visibility")
def set_visibility(ctx, args: dict) -> dict[str, Any]:
    collection = c.collection_arg(args, "collection", required=True)
    recursive = c.optional_bool(args, "recursive", False)
    hide_viewport = c.optional_bool(args, "hide_viewport")
    hide_render = c.optional_bool(args, "hide_render")
    hide_select = c.optional_bool(args, "hide_select")
    exclude = c.optional_bool(args, "exclude")

    targets = [collection, *(_descendants(collection) if recursive else [])]
    for target in targets:
        if hide_viewport is not None:
            target.hide_viewport = hide_viewport
        if hide_render is not None:
            target.hide_render = hide_render
        if hide_select is not None:
            target.hide_select = hide_select
        if exclude is not None:
            layer = _layer_collection(target)
            if layer is None:
                raise BridgeError(
                    ErrorCode.BLENDER_CONTEXT_ERROR,
                    f"`{target.name}` is not linked into the active view layer, so it "
                    "cannot be excluded from it.",
                    {"collection": target.name},
                )
            layer.exclude = exclude

    ctx.bump()
    return {
        "collections": [summarise(target) for target in targets],
        "revision": ctx.revision,
    }

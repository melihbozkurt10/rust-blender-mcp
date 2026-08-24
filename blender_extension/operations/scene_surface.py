"""Mounting surfaces and openings, as structured geometry.

What this is for: placing a camera on the rear wall beside the service door
should not require the caller to work out a point, a normal and a quaternion.
That reasoning belongs in the typed layer, and the typed layer needs the
geometry in a form it can reason about -- not a list of a hundred thousand
triangles.

So nothing here hands out raw mesh access. Faces are grouped into planar
regions, classified by their world-space orientation, and returned with the
frame a placement actually needs: a point, a normal, an in-plane tangent, and
the extent of the region in that frame. The caller asks for a wall; it gets a
wall, in world space, with the object's own rotation already applied.

Three read operations and one narrow write:

* ``scene.surface.inspect``  -- planar regions of one object, grouped and classified
* ``scene.surface.raycast``  -- one ray against named objects
* ``scene.openings.inspect`` -- doors and windows, from authored metadata
* ``scene.openings.mark``    -- tag an object as an opening, with a typed kind

Openings come from metadata somebody authored, never from guessing at gaps in
a mesh. A hole in geometry is not a doorway, and a system that decides it is
will be wrong in a way nobody can debug.
"""

from __future__ import annotations

import math
from typing import Any

import bpy
from mathutils import Matrix, Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import invalid_argument
from . import _common as c

#: Where the opening metadata lives. Fixed keys, written only by
#: ``scene.openings.mark``; nothing here takes a property name from a caller.
OPENING_KIND = "mcp_opening_kind"
OPENING_HOST = "mcp_opening_host"

OPENING_KINDS = ("DOOR", "WINDOW", "SERVICE_DOOR", "UNKNOWN")

SURFACE_CLASSES = ("WALL", "FLOOR", "CEILING", "OTHER")

#: How far from vertical a face may lean and still be a wall, and how far from
#: level to still be a floor or ceiling. Real architecture is rarely exact and
#: an imported building is never exact.
DEFAULT_TILT_DEGREES = 30.0

#: How closely two neighbouring faces must agree to belong to one region.
DEFAULT_NORMAL_TOLERANCE_DEGREES = 8.0
DEFAULT_PLANE_TOLERANCE = 0.02

#: Regions smaller than this are not somewhere anybody mounts anything.
DEFAULT_MIN_AREA = 0.25

#: A ceiling on the work one call does. A building can carry a great many
#: faces and grouping is linear in them, but nothing here should be able to
#: spend a minute inside one request. Hitting it is reported, never silent.
MAX_FACES = 60_000

#: Derived surfaces are cached per object, keyed by everything that could
#: change them.
_CACHE: dict[str, tuple[tuple, dict]] = {}
_CACHE_LIMIT = 32


def _transform_key(obj) -> tuple:
    """A key that changes whenever the object moves, turns or scales."""
    return tuple(round(value, 6) for row in obj.matrix_world for value in row)


def _mesh_key(obj) -> tuple:
    mesh = obj.data
    revision = ids.mesh_revision(mesh) if mesh else 0
    return (
        getattr(mesh, "name", ""),
        revision,
        len(mesh.polygons) if mesh else 0,
        len(mesh.vertices) if mesh else 0,
    )


def _remember(key_id: str, key: tuple, value: dict) -> None:
    if len(_CACHE) >= _CACHE_LIMIT:
        # ponytail: clears wholesale at the cap. Surfaces are cheap to rebuild
        # and the cap is generous; make it an LRU only if a profile says so.
        _CACHE.clear()
    _CACHE[key_id] = (key, value)


def _cached(key_id: str, key: tuple) -> dict | None:
    found = _CACHE.get(key_id)
    if found is None or found[0] != key:
        return None
    return found[1]


def invalidate_surface_cache(object_id: str | None = None) -> None:
    if object_id is None:
        _CACHE.clear()
    else:
        _CACHE.pop(object_id, None)


def _refresh() -> None:
    """Make sure transforms are current before anything reads one.

    ``matrix_world`` is derived from the dependency graph, so an object moved
    or turned earlier in the same session still reports its old matrix until
    the graph is re-evaluated. Reading it stale would silently place a prop on
    the wall's previous orientation -- and the cache would then remember that
    answer under the old key, so it would stay wrong.
    """
    try:
        bpy.context.view_layer.update()
    except (AttributeError, RuntimeError):
        # No view layer, which happens in some headless contexts. The matrices
        # are then whatever they are, and nothing here can do better.
        pass


def _evaluated_mesh(obj):
    """The mesh as the viewport sees it, modifiers included.

    A wall built with a solidify or an array modifier is still a wall, and
    reading the unevaluated mesh would miss it entirely.
    """
    depsgraph = bpy.context.evaluated_depsgraph_get()
    evaluated = obj.evaluated_get(depsgraph)
    try:
        mesh = evaluated.to_mesh()
    except RuntimeError:
        return None, None
    return evaluated, mesh


def _classify(normal: Vector, tilt_degrees: float) -> str:
    """What kind of surface a world-space normal describes."""
    level = math.cos(math.radians(tilt_degrees))
    if normal.z >= level:
        return "FLOOR"
    if normal.z <= -level:
        return "CEILING"
    if abs(normal.z) <= math.sin(math.radians(tilt_degrees)):
        return "WALL"
    return "OTHER"


def _tangent_for(normal: Vector) -> Vector:
    """The surface's own horizontal, pointing the way "right" means.

    Handedness matters here and is easy to get backwards. Somebody standing in
    front of a wall looks along -normal; their right hand points along
    ``up x normal``. Take the other cross product and "right of the door" comes
    out on the left, which is the sort of bug that only shows up in the game.

    Paired with ``bitangent = normal x tangent``, which then points up.
    """
    up = Vector((0.0, 0.0, 1.0))
    tangent = up.cross(normal)
    if tangent.length < 1e-6:
        # The surface is level, so there is no horizontal in it to speak of.
        tangent = Vector((1.0, 0.0, 0.0)).cross(normal)
    if tangent.length < 1e-6:
        tangent = Vector((1.0, 0.0, 0.0))
    return tangent.normalized()


def _world_area(polygon, vertices, matrix) -> float:
    """A face's area after the object's transform, in square metres.

    Newell's method over the world-space corners: it handles an n-gon and a
    non-uniform scale, neither of which the mesh's own area figure survives.
    """
    corners = [matrix @ vertices[index].co for index in polygon.vertices]
    if len(corners) < 3:
        return 0.0
    total = Vector((0.0, 0.0, 0.0))
    for index, current in enumerate(corners):
        following = corners[(index + 1) % len(corners)]
        total += current.cross(following)
    return total.length * 0.5


def _regions_of(obj, tilt: float, normal_tolerance: float, plane_tolerance: float) -> dict:
    """Group an object's faces into planar regions, in world space."""
    evaluated, mesh = _evaluated_mesh(obj)
    if mesh is None:
        raise invalid_argument(
            f"`{obj.name}` has no mesh to read surfaces from.", field="object"
        )
    try:
        polygons = mesh.polygons
        total_faces = len(polygons)
        truncated = total_faces > MAX_FACES
        considered = min(total_faces, MAX_FACES)

        matrix = obj.matrix_world
        # Normals do not transform like points: a non-uniform scale would skew
        # them. This is the matrix that keeps a normal perpendicular.
        normal_matrix = matrix.to_3x3().inverted_safe().transposed()

        vertices = mesh.vertices
        world_normals: list[Vector] = []
        world_centres: list[Vector] = []
        offsets: list[float] = []
        areas: list[float] = []
        for index in range(considered):
            polygon = polygons[index]
            normal = (normal_matrix @ polygon.normal).normalized()
            centre = matrix @ polygon.center
            world_normals.append(normal)
            world_centres.append(centre)
            offsets.append(normal.dot(centre))
            # `polygon.area` is measured in the object's own space, so a
            # scaled object reports a face as its unscaled size -- a 4x3m wall
            # on a unit cube comes back as one square metre. Every threshold
            # here is in metres, so the area has to be measured after the
            # transform.
            areas.append(_world_area(polygon, vertices, matrix))

        # Union-find over faces that share an edge and agree on their plane.
        parent = list(range(considered))

        def find(index: int) -> int:
            while parent[index] != index:
                parent[index] = parent[parent[index]]
                index = parent[index]
            return index

        def union(a: int, b: int) -> None:
            ra, rb = find(a), find(b)
            if ra != rb:
                parent[rb] = ra

        cos_tolerance = math.cos(math.radians(normal_tolerance))
        edge_faces: dict[int, int] = {}
        for index in range(considered):
            for edge_key in polygons[index].edge_keys:
                key = hash(edge_key)
                other = edge_faces.get(key)
                if other is None:
                    edge_faces[key] = index
                    continue
                if (
                    world_normals[index].dot(world_normals[other]) >= cos_tolerance
                    and abs(offsets[index] - offsets[other]) <= plane_tolerance
                ):
                    union(index, other)

        grouped: dict[int, list[int]] = {}
        for index in range(considered):
            grouped.setdefault(find(index), []).append(index)

        regions = []
        for members in grouped.values():
            area = sum(areas[i] for i in members)
            if area <= 0.0:
                continue
            # Area-weighted, so a region's point is where its mass is rather
            # than where its stray slivers are.
            normal = Vector((0.0, 0.0, 0.0))
            centre = Vector((0.0, 0.0, 0.0))
            for i in members:
                normal += world_normals[i] * areas[i]
                centre += world_centres[i] * areas[i]
            if normal.length < 1e-9:
                continue
            normal = normal.normalized()
            centre /= area

            tangent = _tangent_for(normal)
            bitangent = normal.cross(tangent).normalized()

            lo = Vector((float("inf"),) * 3)
            hi = Vector((float("-inf"),) * 3)
            along_min = across_min = float("inf")
            along_max = across_max = float("-inf")
            for i in members:
                for vertex_index in polygons[i].vertices:
                    point = matrix @ vertices[vertex_index].co
                    for axis in range(3):
                        lo[axis] = min(lo[axis], point[axis])
                        hi[axis] = max(hi[axis], point[axis])
                    relative = point - centre
                    along = relative.dot(tangent)
                    across = relative.dot(bitangent)
                    along_min = min(along_min, along)
                    along_max = max(along_max, along)
                    across_min = min(across_min, across)
                    across_max = max(across_max, across)

            regions.append(
                {
                    "classification": _classify(normal, tilt),
                    "face_count": len(members),
                    "area": area,
                    "point": c.vector_dict(centre),
                    "normal": c.vector_dict(normal),
                    "tangent": c.vector_dict(tangent),
                    "bitangent": c.vector_dict(bitangent),
                    "bounds": {"min": c.vector_dict(lo), "max": c.vector_dict(hi)},
                    # The region's own frame, so a caller can say "a third of
                    # the way along" or "60cm to the right" without knowing
                    # anything about the object's rotation.
                    "extent": {
                        "along_min": along_min,
                        "along_max": along_max,
                        "across_min": across_min,
                        "across_max": across_max,
                    },
                    # Where the region sits in the object's own frame, which is
                    # what FRONT/REAR/LEFT/RIGHT mean.
                    "local_point": c.vector_dict(matrix.inverted_safe() @ centre),
                    "local_normal": c.vector_dict(
                        (matrix.to_3x3().inverted_safe() @ normal).normalized()
                    ),
                }
            )

        regions.sort(key=lambda region: region["area"], reverse=True)
        for index, region in enumerate(regions):
            region["region_id"] = index

        world = c.world_bounds([obj])
        return {
            "object": ids.ensure_id(obj),
            "object_name": obj.name,
            "regions": regions,
            "faces_total": total_faces,
            "faces_considered": considered,
            "truncated": truncated,
            "object_bounds": (
                {"min": c.vector_dict(world[0]), "max": c.vector_dict(world[1])}
                if world
                else None
            ),
        }
    finally:
        if evaluated is not None:
            try:
                evaluated.to_mesh_clear()
            except RuntimeError:
                pass


@read("scene.surface.inspect")
def surface_inspect(ctx, args: dict) -> dict[str, Any]:
    """Planar mounting regions of one object, in world space."""
    obj = c.object_arg(args)
    c.require_mesh(obj)
    _refresh()

    tilt = c.optional_float(args, "tilt_degrees", DEFAULT_TILT_DEGREES) or DEFAULT_TILT_DEGREES
    normal_tolerance = (
        c.optional_float(args, "normal_tolerance_degrees", DEFAULT_NORMAL_TOLERANCE_DEGREES)
        or DEFAULT_NORMAL_TOLERANCE_DEGREES
    )
    plane_tolerance = (
        c.optional_float(args, "plane_tolerance", DEFAULT_PLANE_TOLERANCE)
        or DEFAULT_PLANE_TOLERANCE
    )
    minimum_area = c.optional_float(args, "min_area", DEFAULT_MIN_AREA)
    if minimum_area is None:
        minimum_area = DEFAULT_MIN_AREA
    for name, value in (
        ("tilt_degrees", tilt),
        ("normal_tolerance_degrees", normal_tolerance),
        ("plane_tolerance", plane_tolerance),
        ("min_area", minimum_area),
    ):
        c.check_finite(value, name)
        if value < 0.0:
            raise invalid_argument(f"`{name}` cannot be negative.", field=name)
    if tilt >= 90.0:
        raise invalid_argument(
            "`tilt_degrees` at or past 90 would make every face a wall.", field="tilt_degrees"
        )

    wanted = c.optional_str(args, "classification")
    if wanted is not None:
        wanted = c.enum_value(wanted.upper(), SURFACE_CLASSES, "classification")

    object_id = ids.ensure_id(obj)
    key = (_mesh_key(obj), _transform_key(obj), tilt, normal_tolerance, plane_tolerance)
    derived = _cached(object_id, key)
    cache_hit = derived is not None
    if derived is None:
        derived = _regions_of(obj, tilt, normal_tolerance, plane_tolerance)
        _remember(object_id, key, derived)

    regions = [region for region in derived["regions"] if region["area"] >= minimum_area]
    if wanted is not None:
        regions = [region for region in regions if region["classification"] == wanted]
    window, cursor = c.paginate(regions, args)

    return {
        "object": derived["object"],
        "object_name": derived["object_name"],
        "regions": window,
        "total": len(regions),
        "regions_before_filtering": len(derived["regions"]),
        "faces_total": derived["faces_total"],
        "faces_considered": derived["faces_considered"],
        "truncated": derived["truncated"],
        "object_bounds": derived["object_bounds"],
        "cached": cache_hit,
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("scene.surface.raycast")
def surface_raycast(ctx, args: dict) -> dict[str, Any]:
    """One ray against a named set of objects.

    Deliberately not against the whole scene: a caller says what it is asking
    about, and a stray helper object cannot silently become the answer.
    """
    objects = c.objects_arg(args, "objects")
    _refresh()
    origin = c.as_vector(c.require(args, "origin"), "origin")
    direction = c.as_vector(c.require(args, "direction"), "direction")
    if direction.length < 1e-9:
        raise invalid_argument("`direction` has no length.", field="direction")
    direction = direction.normalized()
    distance = c.optional_float(args, "max_distance", 1000.0) or 1000.0
    c.check_finite(distance, "max_distance")
    if distance <= 0.0:
        raise invalid_argument("`max_distance` must be positive.", field="max_distance")

    best = None
    for obj in objects:
        if obj.type != "MESH":
            continue
        matrix = obj.matrix_world
        inverse = matrix.inverted_safe()
        local_origin = inverse @ origin
        # A direction is transformed without the translation, and is not
        # normalised afterwards: `ray_cast` measures distance in local units.
        local_direction = (inverse.to_3x3() @ direction).normalized()
        try:
            hit, location, normal, face_index = obj.ray_cast(
                local_origin, local_direction, distance=distance
            )
        except (RuntimeError, ValueError):
            continue
        if not hit:
            continue
        world_point = matrix @ location
        travelled = (world_point - origin).length
        if travelled > distance:
            continue
        if best is None or travelled < best["distance"]:
            normal_matrix = matrix.to_3x3().inverted_safe().transposed()
            world_normal = (normal_matrix @ normal).normalized()
            best = {
                "object": ids.ensure_id(obj),
                "object_name": obj.name,
                "point": c.vector_dict(world_point),
                "normal": c.vector_dict(world_normal),
                "classification": _classify(world_normal, DEFAULT_TILT_DEGREES),
                "face_index": int(face_index),
                "distance": travelled,
            }

    return {
        "hit": best is not None,
        "result": best,
        "searched": len(objects),
        "revision": ctx.revision,
    }


def _opening_of(obj, host_name: str | None) -> dict[str, Any] | None:
    kind = obj.get(OPENING_KIND)
    if kind is None:
        return None
    host = obj.get(OPENING_HOST) or None
    if host_name is not None and host is not None and host != host_name:
        return None
    bounds = c.world_bounds([obj])
    if bounds is None:
        return None
    lo, hi = bounds
    centre = (lo + hi) * 0.5
    size = hi - lo
    # The thinnest axis of a doorway is the one through the wall, so its
    # normal is that axis. Reported, not asserted: a caller that knows the
    # host wall should use the wall's normal instead.
    thinnest = min(range(3), key=lambda axis: size[axis])
    normal = Vector((0.0, 0.0, 0.0))
    normal[thinnest] = 1.0
    return {
        "id": ids.ensure_id(obj),
        "name": obj.name,
        "kind": kind if kind in OPENING_KINDS else "UNKNOWN",
        "host": host,
        "bounds": {"min": c.vector_dict(lo), "max": c.vector_dict(hi)},
        "centre": c.vector_dict(centre),
        "size": c.vector_dict(size),
        "normal": c.vector_dict(normal),
        "source": "AUTHORED_METADATA",
    }


@read("scene.openings.inspect")
def openings_inspect(ctx, args: dict) -> dict[str, Any]:
    """Doors and windows somebody marked as such.

    Nothing here looks for holes in geometry. An opening is a thing the scene
    says is an opening, and where the scene says nothing the answer is that it
    knows of none.
    """
    _refresh()
    host = c.object_arg(args, "host", required=False)
    explicit = c.objects_arg(args, "objects", required=False)
    host_name = host.name if host is not None else None

    if explicit:
        candidates = explicit
    elif host is not None:
        # Children first, then anything naming this host.
        candidates = [child for child in host.children]
        candidates += [
            obj
            for obj in bpy.data.objects
            if obj not in candidates and obj.get(OPENING_HOST) == host_name
        ]
    else:
        candidates = [obj for obj in bpy.data.objects if OPENING_KIND in obj]

    openings = []
    for obj in candidates:
        found = _opening_of(obj, host_name if explicit else None)
        if found is not None:
            openings.append(found)
    openings.sort(key=lambda opening: opening["name"])

    return {
        "host": ids.ensure_id(host) if host is not None else None,
        "openings": openings,
        "total": len(openings),
        # Said plainly, because "no openings" and "nobody has marked any" are
        # different situations and only one of them is a scene problem.
        "note": (
            "openings are read from authored metadata only; none was found"
            if not openings
            else "openings are read from authored metadata"
        ),
        "revision": ctx.revision,
    }


@op("scene.openings.mark")
def openings_mark(ctx, args: dict) -> dict[str, Any]:
    """Tag an object as a door or window.

    Two fixed property names, one enumerated value. There is no path here that
    writes a property a caller named.
    """
    obj = c.object_arg(args)
    kind = c.enum_value(c.require_str(args, "kind").upper(), OPENING_KINDS, "kind")
    host = c.object_arg(args, "host", required=False)

    obj[OPENING_KIND] = kind
    if host is not None:
        obj[OPENING_HOST] = host.name
    elif OPENING_HOST in obj:
        del obj[OPENING_HOST]

    return {
        "object": ids.ensure_id(obj),
        "name": obj.name,
        "kind": kind,
        "host": host.name if host is not None else None,
        "revision": ctx.revision,
    }

"""Mesh editing.

Everything here goes through ``bmesh.ops`` rather than ``bpy.ops.mesh.*``.
Operators depend on the active editor, the current selection and the current
mode, none of which a bridge should be relying on -- and several of them
(notably loop cut) require a 3D viewport and simply do not exist headless.
``bmesh.ops`` takes explicit element lists, works in background mode, and does
not disturb what the user has selected.
"""

from __future__ import annotations

from typing import Any, Iterable

import bmesh
import bpy
from mathutils import Vector

from .. import ids
from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c


class MeshEdit:
    """A bmesh session over an object mesh.

    Bumps the mesh topology revision on exit, so any indices the caller is
    holding are known to be stale.
    """

    def __init__(self, obj, *, bump_revision: bool = True) -> None:
        self.obj = obj
        self.mesh = c.require_mesh(obj)
        self.bm = None
        self._bump = bump_revision

    def __enter__(self) -> "MeshEdit":
        self.bm = bmesh.new()
        self.bm.from_mesh(self.mesh)
        self.bm.verts.ensure_lookup_table()
        self.bm.edges.ensure_lookup_table()
        self.bm.faces.ensure_lookup_table()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self.bm is None:
            return
        if exc_type is None:
            self.bm.to_mesh(self.mesh)
            self.mesh.update()
            if self._bump:
                ids.next_mesh_revision(self.mesh)
        self.bm.free()
        self.bm = None

    def refresh(self) -> None:
        self.bm.verts.ensure_lookup_table()
        self.bm.edges.ensure_lookup_table()
        self.bm.faces.ensure_lookup_table()

    def elements(self, element_type: str, indices: Iterable[int] | None) -> list:
        """Resolve an element selection to bmesh elements."""
        sequence = {
            "VERTEX": self.bm.verts,
            "EDGE": self.bm.edges,
            "FACE": self.bm.faces,
        }[element_type]
        if not indices:
            return list(sequence)
        resolved = []
        count = len(sequence)
        out_of_range = []
        for index in indices:
            if index < 0 or index >= count:
                out_of_range.append(index)
            else:
                resolved.append(sequence[index])
        if out_of_range:
            raise invalid_argument(
                f"`{self.obj.name}` has {count} {element_type.lower()} elements; "
                f"{len(out_of_range)} of the given indices are out of range.",
                object=self.obj.name,
                element_type=element_type,
                count=count,
                out_of_range=out_of_range[:20],
            )
        return resolved


def selection_args(args: dict, key: str = "selection") -> tuple[str, list[int], int | None]:
    """Unpack an element selection payload."""
    selection = c.optional(args, key)
    if selection is None:
        return "FACE", [], None
    if not isinstance(selection, dict):
        raise invalid_argument(f"`{key}` must be an object.", field=key)
    element_type = c.enum_value(
        str(selection.get("type", "FACE")), ["VERTEX", "EDGE", "FACE"], f"{key}.type"
    )
    indices = [int(i) for i in (selection.get("indices") or [])]
    expected = selection.get("expected_mesh_revision")
    return element_type, indices, int(expected) if expected is not None else None


def require_selection(args: dict, key: str = "selection") -> tuple[str, list[int], int | None]:
    if c.optional(args, key) is None:
        raise invalid_argument(f"`{key}` is required.", field=key)
    return selection_args(args, key)


def _result(obj, ctx, **extra: Any) -> dict[str, Any]:
    mesh = obj.data
    payload = {
        "object": ids.ensure_id(obj),
        "mesh_revision": ids.mesh_revision(mesh),
        "counts": {
            "vertices": len(mesh.vertices),
            "edges": len(mesh.edges),
            "faces": len(mesh.polygons),
        },
        "revision": ctx.revision,
    }
    payload.update(extra)
    return payload


# --- reading ---------------------------------------------------------------


@read("mesh.info")
def info(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    return {
        "object": ids.ensure_id(obj),
        "name": mesh.name,
        "vertices": len(mesh.vertices),
        "edges": len(mesh.edges),
        "faces": len(mesh.polygons),
        "triangles": sum(max(len(p.vertices) - 2, 0) for p in mesh.polygons),
        "mesh_revision": ids.mesh_revision(mesh),
        "uv_maps": [layer.name for layer in mesh.uv_layers],
        "material_slots": [
            slot.material.name if slot.material else None for slot in obj.material_slots
        ],
        "vertex_groups": [group.name for group in obj.vertex_groups],
        "shape_keys": (
            [key.name for key in mesh.shape_keys.key_blocks] if mesh.shape_keys else []
        ),
        "revision": ctx.revision,
    }


@read("mesh.vertices.get")
def get_vertices(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    world_space = c.optional_bool(args, "world_space", False)
    indices = [int(i) for i in c.optional_list(args, "indices")]
    ids.check_mesh_revision(mesh, c.optional_int(args, "expected_mesh_revision"))

    if indices:
        count = len(mesh.vertices)
        bad = [i for i in indices if i < 0 or i >= count]
        if bad:
            raise invalid_argument(
                f"`{obj.name}` has {count} vertices; {len(bad)} indices are out of range.",
                out_of_range=bad[:20],
            )
        selected = [(i, mesh.vertices[i]) for i in indices]
    else:
        selected = list(enumerate(mesh.vertices))

    window, cursor = c.paginate(selected, args)
    matrix = obj.matrix_world
    return {
        "object": ids.ensure_id(obj),
        "mesh_revision": ids.mesh_revision(mesh),
        "vertices": [
            {
                "index": index,
                "co": c.vector_dict(matrix @ vertex.co if world_space else vertex.co),
            }
            for index, vertex in window
        ],
        "total": len(selected),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("mesh.faces.get")
def get_faces(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)
    include_normals = c.optional_bool(args, "include_normals", False)
    indices = [int(i) for i in c.optional_list(args, "indices")]
    ids.check_mesh_revision(mesh, c.optional_int(args, "expected_mesh_revision"))

    polygons = (
        [(i, mesh.polygons[i]) for i in indices] if indices else list(enumerate(mesh.polygons))
    )
    window, cursor = c.paginate(polygons, args)
    faces = []
    for index, polygon in window:
        entry: dict[str, Any] = {
            "index": index,
            "vertices": list(polygon.vertices),
            "material_index": polygon.material_index,
            "area": float(polygon.area),
        }
        if include_normals:
            entry["normal"] = c.vector_dict(polygon.normal)
            entry["center"] = c.vector_dict(polygon.center)
        faces.append(entry)

    return {
        "object": ids.ensure_id(obj),
        "mesh_revision": ids.mesh_revision(mesh),
        "faces": faces,
        "total": len(polygons),
        "next_cursor": cursor,
        "revision": ctx.revision,
    }


@read("mesh.analyze")
def analyze(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mesh = c.require_mesh(obj)

    quads = tris = ngons = degenerate = 0
    for polygon in mesh.polygons:
        sides = len(polygon.vertices)
        if sides == 3:
            tris += 1
        elif sides == 4:
            quads += 1
        else:
            ngons += 1
        if polygon.area < 1e-12:
            degenerate += 1

    with MeshEdit(obj, bump_revision=False) as edit:
        loose_vertices = sum(1 for vertex in edit.bm.verts if not vertex.link_edges)
        loose_edges = sum(1 for edge in edit.bm.edges if not edge.link_faces)
        non_manifold = sum(1 for edge in edit.bm.edges if not edge.is_manifold)
        inverted = _suspect_inverted(edit.bm)

    bounds = c.world_bounds([obj])
    scale = obj.scale
    uniform_scale = (
        abs(scale[0] - 1.0) < 1e-6 and abs(scale[1] - 1.0) < 1e-6 and abs(scale[2] - 1.0) < 1e-6
    )

    return {
        "object": ids.ensure_id(obj),
        "vertices": len(mesh.vertices),
        "edges": len(mesh.edges),
        "faces": len(mesh.polygons),
        "triangles": sum(max(len(p.vertices) - 2, 0) for p in mesh.polygons),
        "mesh_revision": ids.mesh_revision(mesh),
        "bounding_box": (
            {"min": c.vector_dict(bounds[0]), "max": c.vector_dict(bounds[1])} if bounds else None
        ),
        "dimensions": c.vector_dict(c.dimensions_of(obj)),
        "loose_vertices": loose_vertices,
        "loose_edges": loose_edges,
        "non_manifold_edges": non_manifold,
        "degenerate_faces": degenerate,
        "ngons": ngons,
        "quads": quads,
        "tris": tris,
        "suspect_inverted_normals": inverted,
        "uv_maps": [layer.name for layer in mesh.uv_layers],
        "material_slots": [
            slot.material.name if slot.material else None for slot in obj.material_slots
        ],
        "shape_keys": [key.name for key in mesh.shape_keys.key_blocks] if mesh.shape_keys else [],
        "vertex_groups": [group.name for group in obj.vertex_groups],
        "has_applied_scale": uniform_scale,
        "revision": ctx.revision,
    }


def _suspect_inverted(bm) -> int:
    """Count faces whose normal points back towards the mesh centre.

    A heuristic, and named as one: it is reliable for closed convex-ish shells
    and meaningless for open surfaces. It exists so an export check can say
    "these look wrong, go and look", not to decide anything on its own.
    """
    if not bm.faces:
        return 0
    centre = Vector((0.0, 0.0, 0.0))
    for vertex in bm.verts:
        centre += vertex.co
    centre /= max(len(bm.verts), 1)

    suspect = 0
    for face in bm.faces:
        outward = face.calc_center_median() - centre
        if outward.length < 1e-9:
            continue
        if face.normal.dot(outward) < 0:
            suspect += 1
    return suspect


# --- creation --------------------------------------------------------------


@op("mesh.create")
def create(ctx, args: dict) -> dict[str, Any]:
    """Build a mesh from explicit vertex and face lists."""
    name = c.optional_str(args, "name", "Mesh") or "Mesh"
    vertices = c.optional_list(args, "vertices")
    faces = c.optional_list(args, "faces")
    edges = c.optional_list(args, "edges")

    if not vertices:
        raise invalid_argument("`vertices` must contain at least one point.", field="vertices")
    if len(vertices) > 1_000_000:
        raise invalid_argument(
            f"{len(vertices)} vertices is more than one request should carry.",
            field="vertices",
        )

    points = [tuple(c.as_vector(v, "vertices")) for v in vertices]
    face_tuples = []
    for face in faces:
        indices = [int(i) for i in face]
        bad = [i for i in indices if i < 0 or i >= len(points)]
        if bad:
            raise invalid_argument(
                "A face references a vertex index that does not exist.",
                out_of_range=bad[:20],
                vertex_count=len(points),
            )
        if len(indices) < 3:
            raise invalid_argument("A face needs at least three vertices.", face=indices)
        face_tuples.append(tuple(indices))

    edge_tuples = [tuple(int(i) for i in edge) for edge in edges]

    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(points, edge_tuples, face_tuples)
    mesh.update()
    mesh.validate(verbose=False)

    obj = bpy.data.objects.new(name, mesh)
    collection = c.collection_arg(args, "collection") or bpy.context.scene.collection
    collection.objects.link(obj)

    location = c.optional_vector(args, "location")
    if location is not None:
        obj.location = location

    ids.next_mesh_revision(mesh)
    ids.invalidate_cache("object")
    ctx.bump()

    from .object import summarise as summarise_object

    return {"object": summarise_object(obj), "revision": ctx.revision}


# --- editing ---------------------------------------------------------------


@op("mesh.extrude")
def extrude(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    offset = c.optional_vector(args, "offset")
    along_normal = c.optional_float(args, "along_normal")
    individual = c.optional_bool(args, "individual", False)

    if (offset is None) == (along_normal is None):
        raise invalid_argument("Provide exactly one of `offset` or `along_normal`.")

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        elements = edit.elements(element_type, indices)
        if not elements:
            raise invalid_argument("The selection is empty.")

        if element_type == "FACE":
            if individual:
                result = bmesh.ops.extrude_discrete_faces(edit.bm, faces=elements)
                new_faces = result["faces"]
            else:
                result = bmesh.ops.extrude_face_region(edit.bm, geom=elements)
                new_faces = [g for g in result["geom"] if isinstance(g, bmesh.types.BMFace)]
            moved = {v for face in new_faces for v in face.verts}
            _translate(edit.bm, moved, offset, along_normal, new_faces)
            created = len(new_faces)
        elif element_type == "EDGE":
            result = bmesh.ops.extrude_edge_only(edit.bm, edges=elements)
            new_verts = [g for g in result["geom"] if isinstance(g, bmesh.types.BMVert)]
            _translate(edit.bm, set(new_verts), offset, along_normal, [])
            created = len(new_verts)
        else:
            result = bmesh.ops.extrude_vert_indiv(edit.bm, verts=elements)
            new_verts = result["verts"]
            _translate(edit.bm, set(new_verts), offset, along_normal, [])
            created = len(new_verts)

        edit.refresh()

    return _result(obj, ctx, created=created)


def _translate(bm, verts: set, offset: Vector | None, along_normal: float | None, faces) -> None:
    if offset is not None:
        bmesh.ops.translate(bm, verts=list(verts), vec=offset)
        return
    # Along-normal: move each face's own vertices by that face's normal, so a
    # multi-face selection puffs outward rather than sliding in one direction.
    if faces:
        for face in faces:
            bmesh.ops.translate(
                bm, verts=list(face.verts), vec=face.normal * float(along_normal)
            )
    else:
        for vertex in verts:
            vertex.co += vertex.normal * float(along_normal)


@op("mesh.inset")
def inset(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    if element_type != "FACE":
        raise invalid_argument("`mesh.inset` operates on faces.")
    thickness = c.optional_float(args, "thickness", 0.1) or 0.0
    depth = c.optional_float(args, "depth", 0.0) or 0.0
    individual = c.optional_bool(args, "individual", False)

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        faces = edit.elements("FACE", indices)
        if not faces:
            raise invalid_argument("The selection is empty.")
        if individual:
            result = bmesh.ops.inset_individual(
                edit.bm, faces=faces, thickness=thickness, depth=depth
            )
        else:
            result = bmesh.ops.inset_region(
                edit.bm, faces=faces, thickness=thickness, depth=depth, use_boundary=True
            )
        created = len(result.get("faces", []))
        edit.refresh()

    return _result(obj, ctx, created=created)


@op("mesh.bevel")
def bevel(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    amount = c.optional_float(args, "amount", 0.1) or 0.0
    segments = c.optional_int(args, "segments", 1) or 1
    profile = c.optional_float(args, "profile", 0.5)
    clamp = c.optional_bool(args, "clamp_overlap", True)
    offset_type = c.optional_str(args, "offset_type", "OFFSET") or "OFFSET"

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        elements = edit.elements(element_type, indices)
        if element_type == "FACE":
            # Beveling a face selection means beveling its edges, which is what
            # a caller asking to "bevel these faces" means.
            elements = list({edge for face in elements for edge in face.edges})
        elif element_type == "VERTEX":
            result = bmesh.ops.bevel(
                edit.bm,
                geom=elements,
                offset=amount,
                segments=segments,
                profile=profile,
                affect="VERTICES",
                clamp_overlap=bool(clamp),
                offset_type=offset_type,
            )
            edit.refresh()
            return _result(obj, ctx, created=len(result.get("faces", [])))

        if not elements:
            raise invalid_argument("The selection is empty.")
        result = bmesh.ops.bevel(
            edit.bm,
            geom=elements,
            offset=amount,
            segments=segments,
            profile=profile,
            affect="EDGES",
            clamp_overlap=bool(clamp),
            offset_type=offset_type,
        )
        created = len(result.get("faces", []))
        edit.refresh()

    return _result(obj, ctx, created=created)


@op("mesh.subdivide")
def subdivide(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = selection_args(args)
    cuts = c.optional_int(args, "cuts", 1) or 1
    smoothness = c.optional_float(args, "smoothness", 0.0) or 0.0
    use_smooth = c.optional_bool(args, "use_smooth", False)

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        if element_type == "EDGE":
            edges = edit.elements("EDGE", indices)
        elif element_type == "FACE":
            faces = edit.elements("FACE", indices)
            edges = list({edge for face in faces for edge in face.edges})
        else:
            edges = list(edit.bm.edges) if not indices else [
                edge
                for edge in edit.bm.edges
                if all(v.index in set(indices) for v in edge.verts)
            ]
        if not edges:
            raise invalid_argument("Nothing to subdivide in that selection.")

        bmesh.ops.subdivide_edges(
            edit.bm,
            edges=edges,
            cuts=cuts,
            smooth=smoothness if use_smooth else 0.0,
            use_grid_fill=True,
        )
        edit.refresh()

    return _result(obj, ctx)


@op("mesh.loop_cut")
def loop_cut(ctx, args: dict) -> dict[str, Any]:
    """Insert loops across the edge ring containing one edge.

    Blender's own loop-cut operator is modal and needs a 3D viewport, so the
    ring is walked here and the edges are subdivided directly. The result is
    the same geometry the interactive tool produces at factor 0.
    """
    obj = c.object_arg(args)
    edge_index = c.require_int(args, "edge_index")
    cuts = c.optional_int(args, "cuts", 1) or 1
    expected = c.optional_int(args, "expected_mesh_revision")

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        if edge_index < 0 or edge_index >= len(edit.bm.edges):
            raise invalid_argument(
                f"`{obj.name}` has {len(edit.bm.edges)} edges; index {edge_index} is out of range.",
                edge_count=len(edit.bm.edges),
            )
        ring = _edge_ring(edit.bm.edges[edge_index])
        bmesh.ops.subdivide_edges(edit.bm, edges=ring, cuts=cuts, use_grid_fill=True)
        edit.refresh()
        ring_size = len(ring)

    return _result(obj, ctx, ring_size=ring_size, cuts=cuts)


def _edge_ring(edge) -> list:
    """The ring of edges parallel to `edge`, walking across quads."""
    ring = [edge]
    seen = {edge.index}
    for start_face in edge.link_faces:
        current_edge, current_face = edge, start_face
        while True:
            if len(current_face.verts) != 4:
                # Rings are only well defined through quads.
                break
            opposite = _opposite_edge(current_face, current_edge)
            if opposite is None or opposite.index in seen:
                break
            ring.append(opposite)
            seen.add(opposite.index)
            next_faces = [f for f in opposite.link_faces if f != current_face]
            if not next_faces:
                break
            current_edge, current_face = opposite, next_faces[0]
    return ring


def _opposite_edge(face, edge):
    """The edge on the far side of a quad."""
    edges = list(face.edges)
    if len(edges) != 4:
        return None
    index = edges.index(edge)
    return edges[(index + 2) % 4]


@op("mesh.dissolve")
def dissolve(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    mode = c.enum_value(
        c.require_str(args, "mode"), ["VERTICES", "EDGES", "FACES", "LIMITED"], "mode"
    )
    element_type, indices, expected = selection_args(args)
    angle_limit = c.optional_float(args, "angle_limit", 0.0872665)

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        if mode == "LIMITED":
            bmesh.ops.dissolve_limit(
                edit.bm,
                angle_limit=angle_limit,
                verts=list(edit.bm.verts),
                edges=list(edit.bm.edges),
            )
        elif mode == "VERTICES":
            bmesh.ops.dissolve_verts(edit.bm, verts=edit.elements("VERTEX", indices))
        elif mode == "EDGES":
            bmesh.ops.dissolve_edges(edit.bm, edges=edit.elements("EDGE", indices))
        else:
            bmesh.ops.dissolve_faces(edit.bm, faces=edit.elements("FACE", indices))
        edit.refresh()

    return _result(obj, ctx)


@op("mesh.delete_elements")
def delete_elements(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    mode = c.enum_value(
        c.require_str(args, "mode"), ["VERTS", "EDGES", "FACES", "ONLY_FACE", "EDGE_FACE"], "mode"
    )
    if not indices:
        raise invalid_argument(
            "An empty index list means every element; deleting the whole mesh is almost "
            "certainly not intended. Pass explicit indices, or delete the object."
        )

    ids.check_mesh_revision(obj.data, expected)

    context = {
        "VERTS": "VERTS",
        "EDGES": "EDGES",
        "FACES": "FACES",
        "ONLY_FACE": "FACES_ONLY",
        "EDGE_FACE": "EDGES_FACES",
    }[mode]

    with MeshEdit(obj) as edit:
        elements = edit.elements(element_type, indices)
        bmesh.ops.delete(edit.bm, geom=elements, context=context)
        edit.refresh()
        removed = len(elements)

    return _result(obj, ctx, removed=removed)


@op("mesh.merge_vertices")
def merge_vertices(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = selection_args(args)
    distance = c.optional_float(args, "distance", 0.0001) or 0.0001

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        verts = edit.elements("VERTEX", indices) if indices else list(edit.bm.verts)
        before = len(edit.bm.verts)
        bmesh.ops.remove_doubles(edit.bm, verts=verts, dist=distance)
        edit.refresh()
        merged = before - len(edit.bm.verts)

    return _result(obj, ctx, merged=merged)


# `mesh.remove_doubles` is the name most users reach for; it is the same
# operation, registered under both names rather than making callers guess.
@op("mesh.remove_doubles")
def remove_doubles(ctx, args: dict) -> dict[str, Any]:
    return merge_vertices(ctx, args)


@op("mesh.normals.recalculate")
def recalculate_normals(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    inside = c.optional_bool(args, "inside", False)

    with MeshEdit(obj, bump_revision=False) as edit:
        bmesh.ops.recalc_face_normals(edit.bm, faces=list(edit.bm.faces))
        if inside:
            bmesh.ops.reverse_faces(edit.bm, faces=list(edit.bm.faces))

    ctx.bump()
    return _result(obj, ctx, inside=bool(inside))


@op("mesh.normals.flip")
def flip_normals(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = selection_args(args)
    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj, bump_revision=False) as edit:
        faces = edit.elements("FACE", indices) if indices else list(edit.bm.faces)
        bmesh.ops.reverse_faces(edit.bm, faces=faces)
        flipped = len(faces)

    ctx.bump()
    return _result(obj, ctx, flipped=flipped)


@op("mesh.fill")
def fill(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    if element_type != "EDGE":
        raise invalid_argument("`mesh.fill` operates on boundary edges.")
    use_grid = c.optional_bool(args, "use_grid_fill", False)

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        edges = edit.elements("EDGE", indices)
        if not edges:
            raise invalid_argument("The selection is empty.")
        if use_grid:
            result = bmesh.ops.grid_fill(edit.bm, edges=edges, use_interp_simple=True)
        else:
            result = bmesh.ops.contextual_create(edit.bm, geom=edges)
        created = len(result.get("faces", []))
        edit.refresh()

    if created == 0:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            "Those edges do not bound a fillable region. Fill needs a closed loop of boundary "
            "edges.",
            {"object": obj.name, "edge_count": len(indices)},
        )
    return _result(obj, ctx, created=created)


@op("mesh.bridge_edge_loops")
def bridge_edge_loops(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = require_selection(args)
    if element_type != "EDGE":
        raise invalid_argument("`mesh.bridge_edge_loops` operates on edges.")
    if len(indices) < 2:
        raise invalid_argument("Bridging needs edges from at least two loops.")

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        edges = edit.elements("EDGE", indices)
        result = bmesh.ops.bridge_loops(edit.bm, edges=edges)
        created = len(result.get("faces", []))
        edit.refresh()

    if created == 0:
        raise BridgeError(
            ErrorCode.UNSUPPORTED_OPERATION,
            "Those edges do not form two bridgeable loops.",
            {"object": obj.name},
        )
    return _result(obj, ctx, created=created)


@op("mesh.triangulate")
def triangulate(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = selection_args(args)
    quad_method = c.optional_str(args, "quad_method", "BEAUTY") or "BEAUTY"
    ngon_method = c.optional_str(args, "ngon_method", "BEAUTY") or "BEAUTY"

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        faces = edit.elements("FACE", indices) if indices else list(edit.bm.faces)
        bmesh.ops.triangulate(
            edit.bm, faces=faces, quad_method=quad_method, ngon_method=ngon_method
        )
        edit.refresh()

    return _result(obj, ctx)


@op("mesh.quads_from_tris")
def quads_from_tris(ctx, args: dict) -> dict[str, Any]:
    obj = c.object_arg(args)
    element_type, indices, expected = selection_args(args)
    face_angle = c.optional_float(args, "face_angle", 0.698132) or 0.698132
    shape_angle = c.optional_float(args, "shape_angle", 0.698132) or 0.698132

    ids.check_mesh_revision(obj.data, expected)

    with MeshEdit(obj) as edit:
        faces = edit.elements("FACE", indices) if indices else list(edit.bm.faces)
        triangles = [face for face in faces if len(face.verts) == 3]
        if not triangles:
            raise invalid_argument("There are no triangles in that selection to join.")
        before = len(edit.bm.faces)
        bmesh.ops.join_triangles(
            edit.bm,
            faces=triangles,
            angle_face_threshold=face_angle,
            angle_shape_threshold=shape_angle,
            cmp_seam=False,
            cmp_sharp=False,
            cmp_uvs=False,
            cmp_vcols=False,
        )
        edit.refresh()
        joined = before - len(edit.bm.faces)

    return _result(obj, ctx, joined=joined)

//! Mesh editing tools.
//!
//! Mesh elements are addressed by Blender index, which is only valid until the
//! topology changes. Every mutating tool returns a new `mesh_revision`, and
//! every tool that consumes indices accepts `expected_mesh_revision`; sending
//! stale indices is then a clean `TOPOLOGY_STALE` error instead of an edit
//! landing on the wrong faces.

use blender_protocol::{
    command::{Category, OpKind},
    mesh::{
        Bevel, BridgeEdgeLoops, CreateMesh, DeleteElements, Dissolve, Extrude, Fill, FlipNormals,
        GetFaces, GetVertices, Inset, LoopCut, MergeVertices, MeshRefParams, QuadsFromTris,
        RecalculateNormals, Subdivide, Triangulate,
    },
};

use crate::registry::ToolSpec;

const MESH: Category = Category::Mesh;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<MeshRefParams>(
            "mesh.info",
            MESH,
            OpKind::Read,
            "Mesh summary",
            "Element counts, UV maps, material slots, vertex groups, shape keys and the current \
             topology revision for one mesh.",
        ),
        ToolSpec::forward::<MeshRefParams>(
            "mesh.analyze",
            MESH,
            OpKind::Read,
            "Analyse a mesh",
            "Diagnostics for one mesh: loose geometry, non-manifold edges, degenerate faces, ngon \
             and quad counts, suspected inverted normals, bounds and whether the scale is applied. \
             Use this before exporting.",
        ),
        ToolSpec::forward::<GetVertices>(
            "mesh.vertices.get",
            MESH,
            OpKind::Read,
            "Get vertices",
            "Vertex positions, in object or world space, for specific indices or the whole mesh. \
             Paginated, because meshes are large.",
        ),
        ToolSpec::forward::<GetFaces>(
            "mesh.faces.get",
            MESH,
            OpKind::Read,
            "Get faces",
            "Face vertex indices, material index and area, optionally with normals and centres. \
             Paginated.",
        ),
        ToolSpec::forward::<CreateMesh>(
            "mesh.create",
            MESH,
            OpKind::Write,
            "Create a mesh from geometry",
            "Build a mesh object from explicit vertex positions and face index lists. Every index \
             is checked against the vertex count before anything is created.",
        ),
        ToolSpec::forward::<Extrude>(
            "mesh.extrude",
            MESH,
            OpKind::Write,
            "Extrude",
            "Extrude faces, edges or vertices, either by an explicit offset or along each face \
             normal. Individual mode extrudes each face separately instead of as one region.",
        ),
        ToolSpec::forward::<Inset>(
            "mesh.inset",
            MESH,
            OpKind::Write,
            "Inset faces",
            "Inset faces by a thickness, optionally pushing the new face in or out by a depth.",
        ),
        ToolSpec::forward::<Bevel>(
            "mesh.bevel",
            MESH,
            OpKind::Write,
            "Bevel",
            "Bevel edges or vertices with a width, segment count and profile. A face selection is \
             taken to mean the edges of those faces.",
        ),
        ToolSpec::forward::<Subdivide>(
            "mesh.subdivide",
            MESH,
            OpKind::Write,
            "Subdivide",
            "Cut edges to add resolution, optionally smoothing the result. Applies to the whole \
             mesh when no selection is given.",
        ),
        ToolSpec::forward::<LoopCut>(
            "mesh.loop_cut",
            MESH,
            OpKind::Write,
            "Loop cut",
            "Insert edge loops across the ring containing one edge. Rings are walked through \
             quads, so a triangulated mesh has none.",
        ),
        ToolSpec::forward::<Dissolve>(
            "mesh.dissolve",
            MESH,
            OpKind::Write,
            "Dissolve",
            "Remove vertices, edges or faces while keeping the surrounding surface intact, or \
             dissolve everything flatter than an angle limit.",
        ),
        ToolSpec::forward::<DeleteElements>(
            "mesh.delete_elements",
            MESH,
            OpKind::Write,
            "Delete mesh elements",
            "Delete named vertices, edges or faces. An empty index list is refused, because it \
             would mean deleting the entire mesh.",
        ),
        ToolSpec::forward::<MergeVertices>(
            "mesh.merge_vertices",
            MESH,
            OpKind::Write,
            "Merge vertices by distance",
            "Weld vertices closer together than a distance. The same operation is also registered \
             as `mesh.remove_doubles`.",
        ),
        ToolSpec::forward::<MergeVertices>(
            "mesh.remove_doubles",
            MESH,
            OpKind::Write,
            "Remove doubles",
            "Weld duplicate vertices. Identical to `mesh.merge_vertices`, under the name most \
             people look for.",
        ),
        ToolSpec::forward::<RecalculateNormals>(
            "mesh.normals.recalculate",
            MESH,
            OpKind::Write,
            "Recalculate normals",
            "Make face normals consistent, pointing outward by default.",
        ),
        ToolSpec::forward::<FlipNormals>(
            "mesh.normals.flip",
            MESH,
            OpKind::Write,
            "Flip normals",
            "Reverse the winding of selected faces, or of the whole mesh.",
        ),
        ToolSpec::forward::<Fill>(
            "mesh.fill",
            MESH,
            OpKind::Write,
            "Fill a boundary",
            "Create a face spanning a closed loop of boundary edges, or a quad grid across a \
             four-sided boundary.",
        ),
        ToolSpec::forward::<BridgeEdgeLoops>(
            "mesh.bridge_edge_loops",
            MESH,
            OpKind::Write,
            "Bridge edge loops",
            "Connect two edge loops with a band of faces.",
        ),
        ToolSpec::forward::<Triangulate>(
            "mesh.triangulate",
            MESH,
            OpKind::Write,
            "Triangulate",
            "Convert quads and ngons to triangles, which most game engines require.",
        ),
        ToolSpec::forward::<QuadsFromTris>(
            "mesh.quads_from_tris",
            MESH,
            OpKind::Write,
            "Join triangles into quads",
            "Merge adjacent triangles back into quads where the result stays reasonably planar.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_that_takes_indices_offers_a_staleness_check() {
        // The invariant is about *consuming* indices, not about mutating:
        // recalculating normals changes no indices and needs no check, while
        // anything that names elements does.
        let mut checked = 0;
        for tool in tools() {
            let schema = serde_json::to_string(&*tool.schema).unwrap();
            if !schema.contains("\"indices\"") {
                continue;
            }
            checked += 1;
            assert!(
                schema.contains("expected_mesh_revision"),
                "`{}` takes element indices but offers no staleness check",
                tool.name
            );
        }
        assert!(
            checked >= 10,
            "expected most mesh tools to take indices, saw {checked}"
        );
    }

    #[test]
    fn merge_and_remove_doubles_are_the_same_operation() {
        let merge = tools()
            .into_iter()
            .find(|t| t.name == "mesh.merge_vertices")
            .unwrap();
        let doubles = tools()
            .into_iter()
            .find(|t| t.name == "mesh.remove_doubles")
            .unwrap();
        assert_eq!(
            serde_json::to_string(&*merge.schema).unwrap(),
            serde_json::to_string(&*doubles.schema).unwrap()
        );
    }
}

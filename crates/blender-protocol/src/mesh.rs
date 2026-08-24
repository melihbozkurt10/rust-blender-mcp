//! Mesh editing payloads.
//!
//! Element addressing uses Blender's own indices, which are stable only until
//! topology changes. Every operation that can change topology returns a new
//! `mesh_revision`, and every operation that *consumes* indices accepts an
//! `expected_mesh_revision`. Sending stale indices is then a clean
//! `TOPOLOGY_STALE` error instead of silently beveling the wrong edge.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, ErrorCode, Result, Validate,
    ids::ObjectRef,
    math::{Aabb, Finite, Vec3, check_non_negative, check_positive},
};

/// Which mesh element type an operation addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ElementType {
    Vertex,
    Edge,
    Face,
}

/// A set of mesh elements to operate on.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ElementSelection {
    #[serde(rename = "type")]
    pub element_type: ElementType,
    /// Explicit indices. Empty means "every element of this type".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indices: Vec<u32>,
    /// The `mesh_revision` the indices were read at. Strongly recommended:
    /// without it, a mesh edited between the read and the write is silently
    /// operated on with the wrong indices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
}

impl ElementSelection {
    pub fn all(element_type: ElementType) -> Self {
        Self {
            element_type,
            indices: Vec::new(),
            expected_mesh_revision: None,
        }
    }

    pub fn is_everything(&self) -> bool {
        self.indices.is_empty()
    }
}

impl Validate for ElementSelection {
    fn validate(&self) -> Result<()> {
        if self.indices.len() > 1_000_000 {
            return Err(BlenderError::invalid_argument(format!(
                "{} indices is beyond what one request should carry; select by criteria instead.",
                self.indices.len()
            ))
            .with_detail("count", self.indices.len()));
        }
        // Duplicates are not an error in Blender, but they usually mean the
        // caller built the list wrongly, and they inflate the payload.
        let mut sorted = self.indices.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        if sorted.len() != before {
            return Err(BlenderError::invalid_argument(format!(
                "`indices` contains {} duplicate entries.",
                before - sorted.len()
            ))
            .with_detail("duplicates", before - sorted.len()));
        }
        Ok(())
    }
}

/// `mesh.extrude`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Extrude {
    pub object: ObjectRef,
    pub selection: ElementSelection,
    /// Translation applied to the new geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<Vec3>,
    /// Extrude along each face's own normal by this distance. Mutually
    /// exclusive with `offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub along_normal: Option<f64>,
    /// Extrude each face separately rather than as one region.
    #[serde(default)]
    pub individual: bool,
}

impl Validate for Extrude {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        match (self.offset, self.along_normal) {
            (None, None) => Err(BlenderError::invalid_argument(
                "Provide `offset` or `along_normal`.",
            )),
            (Some(_), Some(_)) => Err(BlenderError::invalid_argument(
                "`offset` and `along_normal` cannot both be set.",
            )),
            (Some(offset), None) => offset.check_finite("offset"),
            (None, Some(distance)) => crate::math::check_scalar_finite(distance, "along_normal"),
        }
    }
}

/// `mesh.inset`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Inset {
    pub object: ObjectRef,
    pub selection: ElementSelection,
    pub thickness: f64,
    #[serde(default)]
    pub depth: f64,
    /// Inset each face separately.
    #[serde(default)]
    pub individual: bool,
    /// Keep the original faces as well.
    #[serde(default)]
    pub use_boundary: bool,
}

impl Validate for Inset {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        if self.selection.element_type != ElementType::Face {
            return Err(
                BlenderError::invalid_argument("`mesh.inset` operates on faces.").with_detail(
                    "selection.type",
                    format!("{:?}", self.selection.element_type),
                ),
            );
        }
        check_non_negative(self.thickness, "thickness")?;
        crate::math::check_scalar_finite(self.depth, "depth")
    }
}

/// `mesh.bevel`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Bevel {
    pub object: ObjectRef,
    pub selection: ElementSelection,
    /// Bevel width, interpreted according to `offset_type`.
    pub amount: f64,
    #[serde(default = "default_bevel_segments")]
    pub segments: u32,
    /// 0 = straight chamfer, 1 = fully round.
    #[serde(default = "default_bevel_profile")]
    pub profile: f64,
    /// `OFFSET`, `WIDTH`, `DEPTH` or `PERCENT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_type: Option<String>,
    /// Clamp the bevel so it cannot overlap adjacent geometry.
    #[serde(default = "crate::object::default_true")]
    pub clamp_overlap: bool,
}

fn default_bevel_segments() -> u32 {
    1
}

fn default_bevel_profile() -> f64 {
    0.5
}

impl Validate for Bevel {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        check_positive(self.amount, "amount")?;
        crate::math::check_range(self.profile, 0.0, 1.0, "profile")?;
        if self.segments == 0 || self.segments > 100 {
            return Err(BlenderError::invalid_argument(format!(
                "`segments` must be between 1 and 100, got {}.",
                self.segments
            ))
            .with_detail("field", "segments"));
        }
        if let Some(offset_type) = &self.offset_type {
            const TYPES: [&str; 4] = ["OFFSET", "WIDTH", "DEPTH", "PERCENT"];
            if !TYPES.contains(&offset_type.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "offset_type",
                    offset_type.clone(),
                    TYPES,
                ));
            }
        }
        Ok(())
    }
}

/// `mesh.subdivide`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Subdivide {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    #[serde(default = "default_cuts")]
    pub cuts: u32,
    /// Random displacement applied to new vertices.
    #[serde(default)]
    pub smoothness: f64,
    /// Subdivide with Catmull-Clark smoothing rather than linearly.
    #[serde(default)]
    pub use_smooth: bool,
}

fn default_cuts() -> u32 {
    1
}

impl Validate for Subdivide {
    fn validate(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if self.cuts == 0 || self.cuts > 100 {
            return Err(BlenderError::invalid_argument(format!(
                "`cuts` must be between 1 and 100, got {}.",
                self.cuts
            )));
        }
        crate::math::check_range(self.smoothness, 0.0, 1.0, "smoothness")
    }
}

/// `mesh.loop_cut`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoopCut {
    pub object: ObjectRef,
    /// Edge index that identifies the ring to cut across.
    pub edge_index: u32,
    #[serde(default = "default_cuts")]
    pub cuts: u32,
    /// Slide the new loops along the ring, -1..1.
    #[serde(default)]
    pub factor: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
}

impl Validate for LoopCut {
    fn validate(&self) -> Result<()> {
        if self.cuts == 0 || self.cuts > 100 {
            return Err(BlenderError::invalid_argument(
                "`cuts` must be between 1 and 100.",
            ));
        }
        crate::math::check_range(self.factor, -1.0, 1.0, "factor")
    }
}

/// What `mesh.dissolve` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DissolveMode {
    Vertices,
    Edges,
    Faces,
    /// Remove edges and vertices that lie flat between coplanar faces.
    Limited,
}

/// `mesh.dissolve`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Dissolve {
    pub object: ObjectRef,
    pub mode: DissolveMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    /// Maximum angle in radians for `LIMITED` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_limit: Option<f64>,
}

impl Validate for Dissolve {
    fn validate(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if let Some(angle) = self.angle_limit {
            crate::math::check_range(angle, 0.0, std::f64::consts::PI, "angle_limit")?;
        }
        if self.mode == DissolveMode::Limited && self.angle_limit.is_none() {
            // Blender defaults to 5 degrees; not an error, just noted.
        }
        Ok(())
    }
}

/// How `mesh.delete_elements` treats connected geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeleteMode {
    Verts,
    Edges,
    Faces,
    /// Delete faces but keep their edges and vertices.
    OnlyFace,
    EdgeFace,
}

/// `mesh.delete_elements`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteElements {
    pub object: ObjectRef,
    pub selection: ElementSelection,
    pub mode: DeleteMode,
}

impl Validate for DeleteElements {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        if self.selection.is_everything() {
            return Err(BlenderError::invalid_argument(
                "An empty `indices` list means every element; deleting the whole mesh is almost certainly not intended. Pass explicit indices, or delete the object.",
            ));
        }
        Ok(())
    }
}

/// `mesh.merge_vertices` / `mesh.remove_doubles`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MergeVertices {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    /// Merge vertices closer than this. Blender's default is 0.0001.
    #[serde(default = "default_merge_distance")]
    pub distance: f64,
    /// `CENTER`, `FIRST`, `LAST`, `CURSOR` or `COLLAPSE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Only merge vertices that share an edge.
    #[serde(default)]
    pub only_connected: bool,
}

fn default_merge_distance() -> f64 {
    0.0001
}

impl Validate for MergeVertices {
    fn validate(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        check_non_negative(self.distance, "distance")?;
        if self.distance > 1.0 {
            return Err(BlenderError::invalid_argument(format!(
                "A merge distance of {} collapses most meshes entirely; values above 1.0 are refused.",
                self.distance
            ))
            .with_detail("field", "distance"));
        }
        if let Some(mode) = &self.mode {
            const MODES: [&str; 5] = ["CENTER", "FIRST", "LAST", "CURSOR", "COLLAPSE"];
            if !MODES.contains(&mode.as_str()) {
                return Err(BlenderError::invalid_enum("mode", mode.clone(), MODES));
            }
        }
        Ok(())
    }
}

/// `mesh.bridge_edge_loops`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BridgeEdgeLoops {
    pub object: ObjectRef,
    /// Edges forming the loops to bridge. At least two loops must be present.
    pub selection: ElementSelection,
    #[serde(default)]
    pub cuts: u32,
    #[serde(default)]
    pub smoothness: f64,
    /// Connect the last loop back to the first.
    #[serde(default)]
    pub use_merge: bool,
}

impl Validate for BridgeEdgeLoops {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        if self.selection.element_type != ElementType::Edge {
            return Err(BlenderError::invalid_argument(
                "`mesh.bridge_edge_loops` operates on edges.",
            ));
        }
        if self.selection.indices.len() < 2 {
            return Err(BlenderError::invalid_argument(
                "Bridging needs edges from at least two loops.",
            ));
        }
        Ok(())
    }
}

/// `mesh.triangulate`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Triangulate {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    /// `BEAUTY`, `FIXED`, `FIXED_ALTERNATE` or `SHORTEST_DIAGONAL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quad_method: Option<String>,
    /// `BEAUTY` or `CLIP`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ngon_method: Option<String>,
}

/// `mesh.normals.recalculate`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecalculateNormals {
    pub object: ObjectRef,
    /// Point normals inward instead of outward.
    #[serde(default)]
    pub inside: bool,
}

/// Mesh diagnostics from `mesh.analyze`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MeshAnalysis {
    pub vertices: u64,
    pub edges: u64,
    pub faces: u64,
    pub triangles: u64,
    pub mesh_revision: u64,
    pub bounding_box: Option<Aabb>,
    pub dimensions: Option<Vec3>,
    #[serde(default)]
    pub loose_vertices: u64,
    #[serde(default)]
    pub loose_edges: u64,
    #[serde(default)]
    pub non_manifold_edges: u64,
    #[serde(default)]
    pub degenerate_faces: u64,
    #[serde(default)]
    pub ngons: u64,
    #[serde(default)]
    pub quads: u64,
    #[serde(default)]
    pub tris: u64,
    /// Faces whose normal points away from the mesh's outward direction. A
    /// heuristic, hence the name.
    #[serde(default)]
    pub suspect_inverted_normals: u64,
    #[serde(default)]
    pub uv_maps: Vec<String>,
    #[serde(default)]
    pub material_slots: Vec<String>,
    #[serde(default)]
    pub shape_keys: Vec<String>,
    #[serde(default)]
    pub vertex_groups: Vec<String>,
    /// Whether the object's scale is uniform and applied, which most exporters
    /// care about.
    #[serde(default)]
    pub has_applied_scale: bool,
}

impl MeshAnalysis {
    /// Whether anything here would block a clean game-engine export.
    pub fn export_blockers(&self) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if self.non_manifold_edges > 0 {
            blockers.push("non_manifold_edges");
        }
        if self.degenerate_faces > 0 {
            blockers.push("degenerate_faces");
        }
        if self.loose_vertices > 0 || self.loose_edges > 0 {
            blockers.push("loose_geometry");
        }
        if self.uv_maps.is_empty() {
            blockers.push("no_uv_map");
        }
        if !self.has_applied_scale {
            blockers.push("unapplied_scale");
        }
        blockers
    }
}

/// Helper for the bridge: was a mesh edited since the caller read it?
pub fn check_mesh_revision(expected: Option<u64>, actual: u64) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => Err(BlenderError::new(
            ErrorCode::TopologyStale,
            format!(
                "The mesh changed since these indices were read (expected revision {expected}, mesh is at {actual}). Re-read the mesh and retry."
            ),
        )
        .with_detail("expected_mesh_revision", expected)
        .with_detail("actual_mesh_revision", actual)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(indices: Vec<u32>) -> ElementSelection {
        ElementSelection {
            element_type: ElementType::Face,
            indices,
            expected_mesh_revision: Some(3),
        }
    }

    #[test]
    fn stale_topology_is_detected() {
        assert!(check_mesh_revision(Some(3), 3).is_ok());
        let err = check_mesh_revision(Some(3), 7).unwrap_err();
        assert_eq!(err.code, ErrorCode::TopologyStale);
        assert_eq!(err.details["actual_mesh_revision"], 7);
    }

    #[test]
    fn missing_expectation_is_permitted() {
        assert!(check_mesh_revision(None, 42).is_ok());
    }

    #[test]
    fn duplicate_indices_are_rejected() {
        assert!(selection(vec![1, 2, 2]).validate().is_err());
        assert!(selection(vec![1, 2, 3]).validate().is_ok());
    }

    #[test]
    fn extrude_needs_exactly_one_direction() {
        let base = Extrude {
            object: ObjectRef::name("Cube"),
            selection: selection(vec![0]),
            offset: None,
            along_normal: None,
            individual: false,
        };
        assert!(base.validate().is_err());
        assert!(
            Extrude {
                offset: Some(Vec3::Z),
                ..base.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(
            Extrude {
                offset: Some(Vec3::Z),
                along_normal: Some(1.0),
                ..base
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn deleting_everything_requires_explicit_indices() {
        let params = DeleteElements {
            object: ObjectRef::name("Cube"),
            selection: ElementSelection::all(ElementType::Face),
            mode: DeleteMode::Faces,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn absurd_merge_distances_are_refused() {
        let params = MergeVertices {
            object: ObjectRef::name("Cube"),
            selection: None,
            distance: 5.0,
            mode: None,
            only_connected: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn export_blockers_flag_the_usual_suspects() {
        let analysis = MeshAnalysis {
            non_manifold_edges: 4,
            uv_maps: vec![],
            has_applied_scale: false,
            ..Default::default()
        };
        let blockers = analysis.export_blockers();
        assert!(blockers.contains(&"non_manifold_edges"));
        assert!(blockers.contains(&"no_uv_map"));
        assert!(blockers.contains(&"unapplied_scale"));
    }
}

/// `mesh.info` / `mesh.analyze`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MeshRefParams {
    pub object: ObjectRef,
}

/// `mesh.vertices.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetVertices {
    pub object: ObjectRef,
    /// Specific vertex indices. Empty returns them all, paginated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indices: Vec<u32>,
    /// Report positions in world space rather than object space.
    #[serde(default)]
    pub world_space: bool,
    /// Revision the indices were read at. A read with stale indices returns
    /// the wrong vertices just as silently as a write applies to the wrong
    /// ones, so the check is offered here too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
    #[serde(default, flatten)]
    pub page: crate::Page,
}

/// `mesh.faces.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetFaces {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indices: Vec<u32>,
    /// Include each face normal and centre.
    #[serde(default)]
    pub include_normals: bool,
    /// Revision the indices were read at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
    #[serde(default, flatten)]
    pub page: crate::Page,
}

/// `mesh.create` -- build a mesh from explicit geometry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateMesh {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Vertex positions in object space.
    pub vertices: Vec<Vec3>,
    /// Faces as lists of vertex indices. Three or more indices each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<Vec<u32>>,
    /// Standalone edges, for wireframe or curve-like geometry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<crate::ids::CollectionRef>,
}

/// `mesh.normals.flip`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FlipNormals {
    pub object: ObjectRef,
    /// Faces to flip. Empty flips every face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
}

/// `mesh.fill`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fill {
    pub object: ObjectRef,
    /// Boundary edges enclosing the region to fill.
    pub selection: ElementSelection,
    /// Use grid fill, which produces quads across a four-sided boundary rather
    /// than a single ngon.
    #[serde(default)]
    pub use_grid_fill: bool,
}

/// `mesh.quads_from_tris`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuadsFromTris {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    /// Maximum angle between face normals to join across, in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_angle: Option<f64>,
    /// Maximum shape distortion to accept, in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_angle: Option<f64>,
}

impl Validate for MeshRefParams {}
impl Validate for RecalculateNormals {}

impl Validate for GetVertices {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

impl Validate for GetFaces {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

impl Validate for FlipNormals {
    fn validate(&self) -> Result<()> {
        match &self.selection {
            Some(selection) => selection.validate(),
            None => Ok(()),
        }
    }
}

impl Validate for Fill {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        if self.selection.element_type != ElementType::Edge {
            return Err(BlenderError::invalid_argument(
                "`mesh.fill` operates on edges.",
            ));
        }
        if self.selection.is_everything() {
            return Err(BlenderError::invalid_argument(
                "Filling needs the boundary edges named explicitly.",
            ));
        }
        Ok(())
    }
}

impl Validate for QuadsFromTris {
    fn validate(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        for (value, field) in [
            (self.face_angle, "face_angle"),
            (self.shape_angle, "shape_angle"),
        ] {
            if let Some(v) = value {
                crate::math::check_range(v, 0.0, std::f64::consts::PI, field)?;
            }
        }
        Ok(())
    }
}

impl Validate for Triangulate {
    fn validate(&self) -> Result<()> {
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if let Some(method) = &self.quad_method {
            const METHODS: [&str; 4] = ["BEAUTY", "FIXED", "FIXED_ALTERNATE", "SHORTEST_DIAGONAL"];
            if !METHODS.contains(&method.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "quad_method",
                    method.clone(),
                    METHODS,
                ));
            }
        }
        if let Some(method) = &self.ngon_method {
            const METHODS: [&str; 2] = ["BEAUTY", "CLIP"];
            if !METHODS.contains(&method.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "ngon_method",
                    method.clone(),
                    METHODS,
                ));
            }
        }
        Ok(())
    }
}

impl Validate for CreateMesh {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            crate::check_name(name, "name")?;
        }
        if self.vertices.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`vertices` must contain at least one point.",
            ));
        }
        if self.vertices.len() > 1_000_000 {
            return Err(BlenderError::invalid_argument(format!(
                "{} vertices is more than one request should carry; build the mesh in pieces or \
                 import it from a file.",
                self.vertices.len()
            )));
        }
        for (index, vertex) in self.vertices.iter().enumerate() {
            vertex.check_finite(&format!("vertices[{index}]"))?;
        }
        let count = self.vertices.len() as u32;
        for (index, face) in self.faces.iter().enumerate() {
            if face.len() < 3 {
                return Err(BlenderError::invalid_argument(format!(
                    "faces[{index}] has {} indices; a face needs at least three.",
                    face.len()
                )));
            }
            if let Some(bad) = face.iter().find(|i| **i >= count) {
                return Err(BlenderError::invalid_argument(format!(
                    "faces[{index}] references vertex {bad}, but only {count} were given."
                ))
                .with_detail("face_index", index)
                .with_detail("vertex_count", count));
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            if edge.len() != 2 {
                return Err(BlenderError::invalid_argument(format!(
                    "edges[{index}] has {} indices; an edge needs exactly two.",
                    edge.len()
                )));
            }
            if let Some(bad) = edge.iter().find(|i| **i >= count) {
                return Err(BlenderError::invalid_argument(format!(
                    "edges[{index}] references vertex {bad}, but only {count} were given."
                )));
            }
        }
        if let Some(location) = self.location {
            location.check_finite("location")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod construction_tests {
    use super::*;

    fn mesh(faces: Vec<Vec<u32>>) -> CreateMesh {
        CreateMesh {
            name: None,
            vertices: vec![
                Vec3::ZERO,
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            faces,
            edges: vec![],
            location: None,
            collection: None,
        }
    }

    #[test]
    fn faces_must_reference_real_vertices() {
        assert!(mesh(vec![vec![0, 1, 2]]).validate().is_ok());
        let error = mesh(vec![vec![0, 1, 9]]).validate().unwrap_err();
        assert_eq!(error.details["vertex_count"], 3);
    }

    #[test]
    fn faces_need_three_corners() {
        assert!(mesh(vec![vec![0, 1]]).validate().is_err());
    }

    #[test]
    fn edges_need_exactly_two_ends() {
        let mut params = mesh(vec![]);
        params.edges = vec![vec![0, 1, 2]];
        assert!(params.validate().is_err());
        params.edges = vec![vec![0, 1]];
        assert!(params.validate().is_ok());
    }
}

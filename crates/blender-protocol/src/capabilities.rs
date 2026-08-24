//! What the connected Blender build can actually do.
//!
//! Blender moves fast: render engines get renamed (`BLENDER_EEVEE` ->
//! `BLENDER_EEVEE_NEXT`), importers migrate from `bpy.ops` to extensions,
//! modifiers appear and disappear, node sockets get renamed. Rather than
//! guessing from a version number, the add-on reports what it found by
//! introspecting `bpy` at handshake time, and the server validates against
//! that.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    error::{BlenderError, ErrorCode},
    version::BlenderVersion,
};

/// Capabilities of the connected Blender instance.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    /// Render engine identifiers accepted by `scene.render.engine`.
    #[serde(default)]
    pub render_engines: BTreeSet<String>,
    /// Modifier type identifiers accepted by `object.modifier_add`.
    #[serde(default)]
    pub modifiers: BTreeSet<String>,
    /// Shader node `bl_idname`s registered in this build.
    #[serde(default)]
    pub shader_nodes: BTreeSet<String>,
    /// Geometry node `bl_idname`s registered in this build.
    #[serde(default)]
    pub geometry_nodes: BTreeSet<String>,
    /// Object constraint type identifiers.
    #[serde(default)]
    pub constraints: BTreeSet<String>,
    /// Bone constraint type identifiers.
    #[serde(default)]
    pub bone_constraints: BTreeSet<String>,
    /// Bake pass types supported by the active engine.
    #[serde(default)]
    pub bake_types: BTreeSet<String>,
    /// Image file formats accepted by `image_settings.file_format`.
    #[serde(default)]
    pub image_formats: BTreeSet<String>,
    /// Formats this build can import, keyed by the protocol's format name.
    #[serde(default)]
    pub import_formats: BTreeSet<String>,
    /// Formats this build can export.
    #[serde(default)]
    pub export_formats: BTreeSet<String>,
    /// Coarse feature switches that are not simple identifier lists.
    #[serde(default)]
    pub features: Features,
}

/// Boolean capability switches.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Features {
    #[serde(default)]
    pub cycles: bool,
    #[serde(default)]
    pub eevee: bool,
    #[serde(default)]
    pub workbench: bool,
    #[serde(default)]
    pub geometry_nodes: bool,
    #[serde(default)]
    pub shader_nodes: bool,
    #[serde(default)]
    pub compositor: bool,
    #[serde(default)]
    pub gpu_offscreen_render: bool,
    /// `bpy.ops.ed.undo_push` is usable, which is what atomic batches rely on.
    #[serde(default)]
    pub undo_stack: bool,
    /// Node tree interfaces use the 4.x `interface` API rather than the legacy
    /// `inputs`/`outputs` collections.
    #[serde(default)]
    pub node_tree_interface: bool,
}

/// Identity of the connected Blender, reported once at handshake.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlenderIdentity {
    pub blender_version: BlenderVersion,
    pub python_version: String,
    pub addon_version: String,
    /// `windows`, `linux` or `darwin`.
    pub platform: String,
    /// Whether Blender is running with a UI. Headless instances cannot take
    /// viewport screenshots.
    #[serde(default)]
    pub background: bool,
}

/// Which capability list a lookup should be checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    RenderEngine,
    Modifier,
    ShaderNode,
    GeometryNode,
    Constraint,
    BoneConstraint,
    BakeType,
    ImageFormat,
    ImportFormat,
    ExportFormat,
}

impl CapabilityKind {
    const fn label(self) -> &'static str {
        match self {
            CapabilityKind::RenderEngine => "render engine",
            CapabilityKind::Modifier => "modifier type",
            CapabilityKind::ShaderNode => "shader node type",
            CapabilityKind::GeometryNode => "geometry node type",
            CapabilityKind::Constraint => "object constraint type",
            CapabilityKind::BoneConstraint => "bone constraint type",
            CapabilityKind::BakeType => "bake type",
            CapabilityKind::ImageFormat => "image format",
            CapabilityKind::ImportFormat => "import format",
            CapabilityKind::ExportFormat => "export format",
        }
    }

    const fn error_code(self) -> ErrorCode {
        match self {
            CapabilityKind::ShaderNode | CapabilityKind::GeometryNode => ErrorCode::InvalidNodeType,
            CapabilityKind::ImportFormat
            | CapabilityKind::ExportFormat
            | CapabilityKind::ImageFormat => ErrorCode::UnsupportedFormat,
            _ => ErrorCode::CapabilityUnavailable,
        }
    }
}

impl Capabilities {
    pub fn list(&self, kind: CapabilityKind) -> &BTreeSet<String> {
        match kind {
            CapabilityKind::RenderEngine => &self.render_engines,
            CapabilityKind::Modifier => &self.modifiers,
            CapabilityKind::ShaderNode => &self.shader_nodes,
            CapabilityKind::GeometryNode => &self.geometry_nodes,
            CapabilityKind::Constraint => &self.constraints,
            CapabilityKind::BoneConstraint => &self.bone_constraints,
            CapabilityKind::BakeType => &self.bake_types,
            CapabilityKind::ImageFormat => &self.image_formats,
            CapabilityKind::ImportFormat => &self.import_formats,
            CapabilityKind::ExportFormat => &self.export_formats,
        }
    }

    pub fn supports(&self, kind: CapabilityKind, value: &str) -> bool {
        self.list(kind).contains(value)
    }

    /// Validate a value against a capability list.
    ///
    /// An empty list means the add-on could not introspect that category; the
    /// check passes and Blender remains the authority, because failing closed
    /// would break every operation on an older add-on build.
    pub fn require(&self, kind: CapabilityKind, value: &str) -> Result<(), BlenderError> {
        let list = self.list(kind);
        if list.is_empty() || list.contains(value) {
            return Ok(());
        }
        let suggestions = nearest(value, list, 8);
        Err(BlenderError::new(
            kind.error_code(),
            format!(
                "`{value}` is not an available {label} in the connected Blender build.",
                label = kind.label()
            ),
        )
        .with_detail("requested", value)
        .with_detail("capability", kind.label())
        .with_detail_json("closest_available", &suggestions))
    }
}

/// The `limit` entries in `candidates` most similar to `value`.
///
/// Cheap prefix/substring scoring beats full edit distance here: the failure
/// mode is almost always a near-miss identifier (`ShaderNodeBsdfPrincipal`), and
/// listing several plausible neighbours lets a model fix its own call.
pub fn nearest<'a>(
    value: &str,
    candidates: impl IntoIterator<Item = &'a String>,
    limit: usize,
) -> Vec<String> {
    let needle = value.to_ascii_lowercase();
    let mut scored: Vec<(usize, &'a String)> = candidates
        .into_iter()
        .filter_map(|c| {
            let hay = c.to_ascii_lowercase();
            let score = if hay == needle {
                0
            } else if hay.starts_with(&needle) || needle.starts_with(&hay) {
                1
            } else if hay.contains(&needle) || needle.contains(&hay) {
                2
            } else {
                let shared = common_prefix_len(&hay, &needle);
                if shared >= 4 {
                    3 + (hay.len().abs_diff(needle.len()))
                } else {
                    return None;
                }
            };
            Some((score, c))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, c)| c.clone())
        .collect()
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            modifiers: ["SUBSURF", "BEVEL", "SOLIDIFY"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            shader_nodes: ["ShaderNodeBsdfPrincipled", "ShaderNodeTexImage"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_known_values() {
        assert!(caps().require(CapabilityKind::Modifier, "BEVEL").is_ok());
    }

    #[test]
    fn rejects_unknown_values_with_suggestions() {
        let err = caps()
            .require(CapabilityKind::ShaderNode, "ShaderNodeBsdfPrincipal")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidNodeType);
        let closest = err.details.get("closest_available").unwrap().to_string();
        assert!(
            closest.contains("ShaderNodeBsdfPrincipled"),
            "got {closest}"
        );
    }

    #[test]
    fn empty_capability_list_defers_to_blender() {
        let caps = Capabilities::default();
        assert!(caps.require(CapabilityKind::Modifier, "ANYTHING").is_ok());
    }
}

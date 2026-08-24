//! Export readiness checks.
//!
//! What "ready to export" means depends entirely on where the asset is going.
//! Unreal wants centimetres and -Y forward; glTF wants +Y up and triangles;
//! a generic OBJ wants none of that. Encoding those differences as profiles,
//! rather than as advice in a docstring, is what makes the check useful.

use blender_protocol::{
    io::{ExportProfile, Finding, Severity},
    mesh::MeshAnalysis,
};

/// One object's state, as far as an export check cares.
#[derive(Debug, Clone)]
pub struct ObjectReport {
    pub name: String,
    pub analysis: MeshAnalysis,
    /// Object scale, to check it has been applied.
    pub scale: [f64; 3],
    /// Material slots that hold nothing.
    pub empty_material_slots: usize,
    /// Images this object's materials reference that are missing from disk.
    pub missing_textures: Vec<String>,
    /// Whether the object has an armature modifier.
    pub is_skinned: bool,
    /// The largest number of bones influencing any one vertex.
    pub max_bone_influences: Option<u32>,
}

/// What a profile requires.
#[derive(Debug, Clone, Copy)]
pub struct ProfileRules {
    pub requires_triangles: bool,
    pub requires_uvs: bool,
    pub requires_applied_scale: bool,
    /// Maximum bone influences per vertex, where the target engine has a limit.
    pub max_bone_influences: Option<u32>,
    /// Unit scale relative to metres.
    pub unit_scale: f64,
    pub forward_axis: &'static str,
    pub up_axis: &'static str,
    /// Whether names must avoid spaces and punctuation.
    pub strict_names: bool,
}

/// The rules for a profile.
pub fn rules_for(profile: ExportProfile) -> ProfileRules {
    let (forward, up) = profile.axes();
    match profile {
        ExportProfile::Generic => ProfileRules {
            requires_triangles: false,
            requires_uvs: false,
            requires_applied_scale: false,
            max_bone_influences: None,
            unit_scale: 1.0,
            forward_axis: forward,
            up_axis: up,
            strict_names: false,
        },
        ExportProfile::GameAsset => ProfileRules {
            requires_triangles: true,
            requires_uvs: true,
            requires_applied_scale: true,
            max_bone_influences: Some(4),
            unit_scale: 1.0,
            forward_axis: forward,
            up_axis: up,
            strict_names: true,
        },
        ExportProfile::Gltf => ProfileRules {
            requires_triangles: true,
            requires_uvs: true,
            requires_applied_scale: true,
            max_bone_influences: Some(4),
            unit_scale: 1.0,
            forward_axis: forward,
            up_axis: up,
            strict_names: false,
        },
        ExportProfile::Unreal => ProfileRules {
            requires_triangles: true,
            requires_uvs: true,
            requires_applied_scale: true,
            max_bone_influences: Some(8),
            unit_scale: 100.0,
            forward_axis: forward,
            up_axis: up,
            strict_names: true,
        },
        ExportProfile::Unity => ProfileRules {
            requires_triangles: true,
            requires_uvs: true,
            requires_applied_scale: true,
            max_bone_influences: Some(4),
            unit_scale: 1.0,
            forward_axis: forward,
            up_axis: up,
            strict_names: true,
        },
    }
}

/// Check one object against a profile.
pub fn check_object(report: &ObjectReport, profile: ExportProfile) -> Vec<Finding> {
    let rules = rules_for(profile);
    let mut findings = Vec::new();

    let scale_applied = report.scale.iter().all(|value| (value - 1.0).abs() < 1e-4);
    if rules.requires_applied_scale && !scale_applied {
        findings.push(finding(
            Severity::Error,
            "UNAPPLIED_SCALE",
            &report.name,
            format!(
                "`{}` has an object scale of {:?}, which most importers bake in wrongly.",
                report.name, report.scale
            ),
            "Run scene.apply_transforms with scale:true.",
            true,
        ));
    }

    if rules.requires_uvs && report.analysis.uv_maps.is_empty() {
        findings.push(finding(
            Severity::Error,
            "NO_UV_MAP",
            &report.name,
            format!(
                "`{}` has no UV map, so it cannot carry textures.",
                report.name
            ),
            "Unwrap it with uv.smart_project.",
            false,
        ));
    }

    if rules.requires_triangles && (report.analysis.ngons > 0 || report.analysis.quads > 0) {
        findings.push(finding(
            Severity::Warning,
            "NOT_TRIANGULATED",
            &report.name,
            format!(
                "`{}` has {} quads and {} ngons; the target expects triangles and will \
                 triangulate them itself, possibly differently from Blender.",
                report.name, report.analysis.quads, report.analysis.ngons
            ),
            "Run mesh.triangulate, or export with triangulate:true.",
            true,
        ));
    }

    if report.analysis.non_manifold_edges > 0 {
        findings.push(finding(
            Severity::Warning,
            "NON_MANIFOLD",
            &report.name,
            format!(
                "`{}` has {} non-manifold edges, which break normals, boolean operations and most \
                 physics.",
                report.name, report.analysis.non_manifold_edges
            ),
            "Inspect with mesh.analyze; merge vertices or delete the loose geometry.",
            false,
        ));
    }

    if report.analysis.degenerate_faces > 0 {
        findings.push(finding(
            Severity::Warning,
            "DEGENERATE_FACES",
            &report.name,
            format!(
                "`{}` has {} zero-area faces, which produce invalid normals.",
                report.name, report.analysis.degenerate_faces
            ),
            "Run mesh.merge_vertices to weld them away.",
            true,
        ));
    }

    if report.analysis.loose_vertices > 0 || report.analysis.loose_edges > 0 {
        findings.push(finding(
            Severity::Info,
            "LOOSE_GEOMETRY",
            &report.name,
            format!(
                "`{}` has {} loose vertices and {} loose edges, which most formats simply drop.",
                report.name, report.analysis.loose_vertices, report.analysis.loose_edges
            ),
            "Run scene.cleanup with remove_loose_geometry:true.",
            true,
        ));
    }

    if report.empty_material_slots > 0 {
        findings.push(finding(
            Severity::Info,
            "EMPTY_MATERIAL_SLOTS",
            &report.name,
            format!(
                "`{}` has {} empty material slots, which some importers turn into blank materials.",
                report.name, report.empty_material_slots
            ),
            "Run scene.cleanup with remove_unused_material_slots:true.",
            true,
        ));
    }

    if !report.missing_textures.is_empty() {
        findings.push(finding(
            Severity::Error,
            "MISSING_TEXTURES",
            &report.name,
            format!(
                "`{}` references {} texture file(s) that are not on disk.",
                report.name,
                report.missing_textures.len()
            ),
            "Find them with scene.find_missing_textures and reload or repath them.",
            false,
        ));
    }

    if let (Some(limit), Some(actual)) = (rules.max_bone_influences, report.max_bone_influences)
        && report.is_skinned
        && actual > limit
    {
        findings.push(finding(
            Severity::Error,
            "TOO_MANY_INFLUENCES",
            &report.name,
            format!(
                "`{}` has vertices influenced by {actual} bones; this target allows {limit}.",
                report.name
            ),
            "Run rig.fix.normalize_weights with max_influences set to the limit.",
            true,
        ));
    }

    if rules.strict_names && !is_clean_name(&report.name) {
        findings.push(finding(
            Severity::Warning,
            "UNSAFE_NAME",
            &report.name,
            format!(
                "`{}` contains characters that some engines rename on import, which breaks any \
                 reference to it.",
                report.name
            ),
            "Rename it with scene.batch_rename.",
            true,
        ));
    }

    findings
}

/// Whether a name survives a round trip through a typical game engine.
pub fn is_clean_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The worst severity in a set of findings.
pub fn worst(findings: &[Finding]) -> Option<Severity> {
    findings.iter().map(|f| f.severity).max()
}

/// Whether the findings would block an export.
pub fn blocks_export(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

fn finding(
    severity: Severity,
    code: &str,
    entity: &str,
    message: String,
    fix: &str,
    auto_fixable: bool,
) -> Finding {
    Finding {
        severity,
        code: code.to_string(),
        entity: Some(entity.to_string()),
        message,
        suggested_fix: Some(fix.to_string()),
        auto_fixable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> ObjectReport {
        ObjectReport {
            name: "SM_Crate".into(),
            analysis: MeshAnalysis {
                vertices: 8,
                edges: 12,
                faces: 12,
                triangles: 12,
                tris: 12,
                uv_maps: vec!["UVMap".into()],
                has_applied_scale: true,
                ..Default::default()
            },
            scale: [1.0, 1.0, 1.0],
            empty_material_slots: 0,
            missing_textures: vec![],
            is_skinned: false,
            max_bone_influences: None,
        }
    }

    #[test]
    fn a_clean_object_passes_a_strict_profile() {
        let findings = check_object(&clean(), ExportProfile::Unreal);
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
        assert!(!blocks_export(&findings));
    }

    #[test]
    fn the_generic_profile_demands_almost_nothing() {
        let mut report = clean();
        report.scale = [2.0, 2.0, 2.0];
        report.analysis.uv_maps.clear();
        report.analysis.quads = 6;
        let findings = check_object(&report, ExportProfile::Generic);
        assert!(
            findings.is_empty(),
            "generic should not object: {findings:?}"
        );
    }

    #[test]
    fn unapplied_scale_blocks_a_game_export() {
        let mut report = clean();
        report.scale = [2.0, 1.0, 1.0];
        let findings = check_object(&report, ExportProfile::GameAsset);
        assert!(findings.iter().any(|f| f.code == "UNAPPLIED_SCALE"));
        assert!(blocks_export(&findings));
    }

    #[test]
    fn missing_uvs_block_a_textured_target() {
        let mut report = clean();
        report.analysis.uv_maps.clear();
        let findings = check_object(&report, ExportProfile::Gltf);
        let uv = findings
            .iter()
            .find(|f| f.code == "NO_UV_MAP")
            .expect("uv finding");
        assert_eq!(uv.severity, Severity::Error);
        assert!(
            !uv.auto_fixable,
            "unwrapping is a judgement call, not an automatic fix"
        );
    }

    #[test]
    fn quads_are_a_warning_not_a_blocker() {
        let mut report = clean();
        report.analysis.quads = 6;
        let findings = check_object(&report, ExportProfile::Unity);
        let tri = findings
            .iter()
            .find(|f| f.code == "NOT_TRIANGULATED")
            .expect("finding");
        assert_eq!(tri.severity, Severity::Warning);
        assert!(!blocks_export(&findings));
    }

    #[test]
    fn too_many_influences_only_matters_for_skinned_meshes() {
        let mut report = clean();
        report.max_bone_influences = Some(8);
        let findings = check_object(&report, ExportProfile::Unity);
        assert!(!findings.iter().any(|f| f.code == "TOO_MANY_INFLUENCES"));

        report.is_skinned = true;
        let findings = check_object(&report, ExportProfile::Unity);
        assert!(findings.iter().any(|f| f.code == "TOO_MANY_INFLUENCES"));
    }

    #[test]
    fn unreal_allows_more_influences_than_unity() {
        let mut report = clean();
        report.is_skinned = true;
        report.max_bone_influences = Some(6);
        assert!(
            check_object(&report, ExportProfile::Unreal)
                .iter()
                .all(|f| f.code != "TOO_MANY_INFLUENCES")
        );
        assert!(
            check_object(&report, ExportProfile::Unity)
                .iter()
                .any(|f| f.code == "TOO_MANY_INFLUENCES")
        );
    }

    #[test]
    fn awkward_names_are_flagged_only_by_strict_profiles() {
        let mut report = clean();
        report.name = "My Crate (final)".into();
        assert!(
            check_object(&report, ExportProfile::GameAsset)
                .iter()
                .any(|f| f.code == "UNSAFE_NAME")
        );
        assert!(
            check_object(&report, ExportProfile::Gltf)
                .iter()
                .all(|f| f.code != "UNSAFE_NAME")
        );
    }

    #[test]
    fn missing_textures_block_every_profile_that_carries_them() {
        let mut report = clean();
        report.missing_textures = vec!["wood_diffuse.png".into()];
        let findings = check_object(&report, ExportProfile::Generic);
        assert!(blocks_export(&findings));
    }

    #[test]
    fn unreal_expects_centimetres() {
        assert_eq!(rules_for(ExportProfile::Unreal).unit_scale, 100.0);
        assert_eq!(rules_for(ExportProfile::Unity).unit_scale, 1.0);
        assert_eq!(rules_for(ExportProfile::Gltf).up_axis, "Y");
    }

    #[test]
    fn severity_ordering_is_usable() {
        let findings = check_object(
            &ObjectReport {
                analysis: MeshAnalysis {
                    loose_vertices: 2,
                    ..clean().analysis
                },
                ..clean()
            },
            ExportProfile::Generic,
        );
        assert_eq!(worst(&findings), Some(Severity::Info));
    }

    #[test]
    fn clean_name_rules_are_what_they_say() {
        assert!(is_clean_name("SM_Wall_01"));
        assert!(is_clean_name("wall.001"));
        assert!(!is_clean_name("wall 01"));
        assert!(!is_clean_name("wall(final)"));
        assert!(!is_clean_name(""));
    }
}

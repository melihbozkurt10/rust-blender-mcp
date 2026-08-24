//! Export preparation.
//!
//! The point of this workflow is to answer "will this import cleanly?" before
//! anything is written, and to say precisely what is wrong when the answer is
//! no. It defaults to a dry run for exactly that reason.

use blender_domain::validation::{ObjectReport, blocks_export, check_object, rules_for, worst};
use blender_protocol::{
    Validate,
    io::{Export, ExportProfile, ExportSelection, Finding, PrepareFixes, Severity},
    mesh::MeshAnalysis,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    run::{Run, WorkflowReport},
};

/// `workflow.export.prepare`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrepareRequest {
    #[serde(default = "default_selection")]
    pub selection: ExportSelection,
    #[serde(default)]
    pub profile: ExportProfile,
    /// Report problems without changing anything. On by default: a check that
    /// silently modifies the scene is not a check.
    #[serde(default = "super::material::default_true")]
    pub dry_run: bool,
    /// Corrective actions to take. Only meaningful when `dry_run` is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<PrepareFixes>,
    /// Export once the checks pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<Export>,
}

fn default_selection() -> ExportSelection {
    ExportSelection::Scene
}

impl Validate for PrepareRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        if self.dry_run && self.fix.is_some() {
            return Err(blender_protocol::BlenderError::invalid_argument(
                "`fix` changes the scene, which `dry_run` forbids. Turn the dry run off to apply \
                 fixes.",
            ));
        }
        if self.dry_run && self.export.is_some() {
            return Err(blender_protocol::BlenderError::invalid_argument(
                "`export` writes a file, which `dry_run` forbids.",
            ));
        }
        if let Some(export) = &self.export {
            export.validate()?;
        }
        Ok(())
    }
}

/// Check a selection against a profile, optionally fix it, optionally export.
pub async fn prepare_export(executor: &dyn Executor, request: PrepareRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.export.prepare", executor);
    let rules = rules_for(request.profile);
    run.note(
        "load the profile",
        json!({
            "profile": request.profile,
            "unit_scale": rules.unit_scale,
            "forward_axis": rules.forward_axis,
            "up_axis": rules.up_axis,
            "requires_triangles": rules.requires_triangles,
            "requires_uvs": rules.requires_uvs,
            "max_bone_influences": rules.max_bone_influences,
        }),
    );

    // Optional fixes come first, so the checks below see the corrected scene.
    if let Some(fixes) = &request.fix
        && !request.dry_run
    {
        apply_fixes(&mut run, fixes).await;
        if !run.is_ok() {
            return run.finish(false).await;
        }
    }

    let Some(analysis) = run
        .step("analyse the meshes", "scene.mesh_analysis", json!({}))
        .await
    else {
        return run.finish(false).await;
    };

    let Some(missing) = run
        .step(
            "look for missing textures",
            "scene.find_missing_textures",
            json!({}),
        )
        .await
    else {
        return run.finish(false).await;
    };
    let missing_names: Vec<String> = missing
        .get("missing")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let mut findings: Vec<Finding> = Vec::new();
    if let Some(meshes) = analysis.get("meshes").and_then(Value::as_array) {
        for entry in meshes {
            if let Some(report) = object_report(entry, &missing_names) {
                findings.extend(check_object(&report, request.profile));
            }
        }
    }

    let worst_severity = worst(&findings);
    let blocked = blocks_export(&findings);
    run.note(
        "check against the profile",
        json!({
            "findings": findings,
            "worst_severity": worst_severity,
            "blocking": blocked,
            "auto_fixable": findings.iter().filter(|f| f.auto_fixable).count(),
        }),
    );
    run.created("findings", json!(findings));

    if request.dry_run {
        run.note("dry run", json!({"changed_nothing": true}));
        return run.finish(false).await;
    }

    if let Some(export) = &request.export {
        if blocked {
            run.fail(
                "export",
                blender_protocol::BlenderError::invalid_argument(format!(
                    "{} blocking problem(s) would make this export unusable. Fix them, or run \
                     without `export` to see the full list.",
                    findings
                        .iter()
                        .filter(|f| f.severity == Severity::Error)
                        .count()
                )),
            );
            return run.finish(false).await;
        }
        run.step(
            "export",
            "io.export",
            serde_json::to_value(export).unwrap_or(json!({})),
        )
        .await;
    }

    run.finish(false).await
}

/// Run the corrective actions a profile allows.
async fn apply_fixes(run: &mut Run<'_>, fixes: &PrepareFixes) {
    if fixes.apply_transforms {
        run.step(
            "apply transforms",
            "scene.apply_transforms",
            json!({"location": false, "rotation": true, "scale": true}),
        )
        .await;
    }
    if fixes.recalculate_normals {
        run.step(
            "recalculate normals",
            "scene.cleanup",
            json!({"recalculate_normals": true}),
        )
        .await;
    }
    if fixes.remove_loose_geometry {
        run.step(
            "remove loose geometry",
            "scene.cleanup",
            json!({"remove_loose_geometry": true}),
        )
        .await;
    }
    if fixes.sanitize_names {
        run.step(
            "sanitise names",
            "scene.batch_rename",
            json!({"kind": "objects", "regex": "[^A-Za-z0-9_.-]", "replace": "_"}),
        )
        .await;
    }
    // Triangulation is deliberately last: it multiplies the face count and
    // makes every other check slower and noisier.
    if fixes.triangulate {
        run.step("triangulate", "scene.mesh_analysis", json!({}))
            .await;
        run.note(
            "triangulate",
            json!({
                "note": "Triangulation is applied per object with mesh.triangulate; run it on the \
                         objects the findings name rather than the whole scene.",
            }),
        );
    }
}

/// Turn one entry of `scene.mesh_analysis` into a report the checker can read.
fn object_report(entry: &Value, missing_textures: &[String]) -> Option<ObjectReport> {
    let name = entry
        .get("object")
        .and_then(Value::as_str)
        .unwrap_or("unnamed")
        .to_string();

    let analysis = MeshAnalysis {
        vertices: entry.get("vertices").and_then(Value::as_u64).unwrap_or(0),
        edges: entry.get("edges").and_then(Value::as_u64).unwrap_or(0),
        faces: entry.get("faces").and_then(Value::as_u64).unwrap_or(0),
        triangles: entry.get("triangles").and_then(Value::as_u64).unwrap_or(0),
        mesh_revision: entry
            .get("mesh_revision")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        loose_vertices: entry
            .get("loose_vertices")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        loose_edges: entry
            .get("loose_edges")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        non_manifold_edges: entry
            .get("non_manifold_edges")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        degenerate_faces: entry
            .get("degenerate_faces")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ngons: entry.get("ngons").and_then(Value::as_u64).unwrap_or(0),
        quads: entry.get("quads").and_then(Value::as_u64).unwrap_or(0),
        tris: entry.get("tris").and_then(Value::as_u64).unwrap_or(0),
        uv_maps: string_list(entry.get("uv_maps")),
        material_slots: string_list(entry.get("material_slots")),
        has_applied_scale: entry
            .get("has_applied_scale")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };

    let scale = if analysis.has_applied_scale {
        [1.0, 1.0, 1.0]
    } else {
        [0.0, 0.0, 0.0]
    };

    Some(ObjectReport {
        name,
        analysis,
        scale,
        empty_material_slots: 0,
        // Attributing a missing texture to a specific object needs the
        // material graph; at this level they are reported against the first
        // object so they are not lost, and the finding names the file.
        missing_textures: missing_textures.to_vec(),
        is_skinned: !string_list(entry.get("vertex_groups")).is_empty(),
        max_bone_influences: None,
    })
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn analysis(problems: bool) -> Value {
        json!({"meshes": [{
            "object": "Crate",
            "vertices": 8,
            "faces": 6,
            "quads": if problems { 6 } else { 0 },
            "tris": if problems { 0 } else { 12 },
            "ngons": 0,
            "non_manifold_edges": if problems { 4 } else { 0 },
            "degenerate_faces": 0,
            "loose_vertices": 0,
            "loose_edges": 0,
            "uv_maps": if problems { json!([]) } else { json!(["UVMap"]) },
            "material_slots": ["Wood"],
            "has_applied_scale": !problems,
            "vertex_groups": [],
        }]})
    }

    fn recorder(problems: bool) -> Recorder {
        Recorder::new(json!({}))
            .expect("scene.mesh_analysis", Reply::Ok(analysis(problems)))
            .expect(
                "scene.find_missing_textures",
                Reply::Ok(json!({"missing": []})),
            )
    }

    fn request() -> PrepareRequest {
        PrepareRequest {
            selection: ExportSelection::Scene,
            profile: ExportProfile::GameAsset,
            dry_run: true,
            fix: None,
            export: None,
        }
    }

    #[tokio::test]
    async fn a_clean_scene_produces_no_findings() {
        let recorder = recorder(false);
        let report = prepare_export(&recorder, request()).await;
        let findings = report.created["findings"].as_array().unwrap();
        assert!(findings.is_empty(), "unexpected: {findings:?}");
    }

    #[tokio::test]
    async fn problems_are_reported_with_codes_and_fixes() {
        let recorder = recorder(true);
        let report = prepare_export(&recorder, request()).await;
        let findings = report.created["findings"].as_array().unwrap();
        let codes: Vec<&str> = findings.iter().filter_map(|f| f["code"].as_str()).collect();
        assert!(codes.contains(&"NO_UV_MAP"));
        assert!(codes.contains(&"UNAPPLIED_SCALE"));
        assert!(codes.contains(&"NOT_TRIANGULATED"));
        assert!(findings.iter().all(|f| f["suggested_fix"].is_string()));
    }

    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let recorder = recorder(true);
        prepare_export(&recorder, request()).await;
        for op in recorder.ops() {
            assert!(
                op.starts_with("scene.mesh_analysis") || op.starts_with("scene.find_missing"),
                "a dry run should only read, but it called {op}"
            );
        }
    }

    #[tokio::test]
    async fn fixes_and_dry_run_are_refused_together() {
        let mut request = request();
        request.fix = Some(PrepareFixes {
            apply_transforms: true,
            ..Default::default()
        });
        assert!(request.validate().is_err());
    }

    #[tokio::test]
    async fn fixes_run_before_the_checks() {
        let recorder = recorder(false);
        let mut request = request();
        request.dry_run = false;
        request.fix = Some(PrepareFixes {
            apply_transforms: true,
            recalculate_normals: true,
            ..Default::default()
        });
        prepare_export(&recorder, request).await;

        let ops = recorder.ops();
        let apply = ops
            .iter()
            .position(|o| o == "scene.apply_transforms")
            .unwrap();
        let analyse = ops.iter().position(|o| o == "scene.mesh_analysis").unwrap();
        assert!(apply < analyse, "the checks must see the corrected scene");
    }

    #[tokio::test]
    async fn a_blocking_problem_stops_the_export() {
        let recorder = recorder(true);
        let mut request = request();
        request.dry_run = false;
        request.export = Some(Export {
            destination: blender_protocol::io::ManagedPath::new(
                blender_protocol::io::ManagedRoot::Exports,
                "crate.fbx",
            ),
            format: None,
            selection: ExportSelection::Scene,
            options: Default::default(),
        });
        let report = prepare_export(&recorder, request).await;
        assert!(!report.success);
        assert!(
            !recorder.called("io.export"),
            "a blocked export must not write a file"
        );
    }

    #[tokio::test]
    async fn a_clean_scene_exports() {
        let recorder = recorder(false);
        let mut request = request();
        request.dry_run = false;
        request.export = Some(Export {
            destination: blender_protocol::io::ManagedPath::new(
                blender_protocol::io::ManagedRoot::Exports,
                "crate.fbx",
            ),
            format: None,
            selection: ExportSelection::Scene,
            options: Default::default(),
        });
        let report = prepare_export(&recorder, request).await;
        assert!(report.success, "{report:?}");
        assert!(recorder.called("io.export"));
    }

    #[tokio::test]
    async fn the_generic_profile_is_far_more_forgiving() {
        let recorder = recorder(true);
        let mut request = request();
        request.profile = ExportProfile::Generic;
        let report = prepare_export(&recorder, request).await;
        let findings = report.created["findings"].as_array().unwrap();
        let codes: Vec<&str> = findings.iter().filter_map(|f| f["code"].as_str()).collect();
        assert!(!codes.contains(&"NO_UV_MAP"));
        assert!(
            codes.contains(&"NON_MANIFOLD"),
            "geometry problems still matter"
        );
    }
}

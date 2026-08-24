//! Material workflows.
//!
//! Every one of these is: plan the graph in Rust, create the material, send the
//! graph as one declarative build, assign it. Four operations regardless of how
//! many texture maps are involved, rather than one per node and link.

use blender_domain::material::{EmissiveSpec, GlassSpec, PbrSpec};
use blender_protocol::{BlenderError, ids::ObjectRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    run::{Run, WorkflowReport},
};

/// `workflow.material.pbr`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PbrRequest {
    /// Name for the material.
    pub name: String,
    #[serde(flatten)]
    pub spec: PbrSpec,
    /// Objects to assign the finished material to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assign_to: Vec<ObjectRef>,
    /// Remove what was created if any step fails.
    #[serde(default = "default_true")]
    pub rollback_on_failure: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

impl blender_protocol::Validate for PbrRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        blender_protocol::check_name(&self.name, "name")?;
        // Planning the graph is the validation: if it cannot be planned, the
        // request is not buildable.
        self.spec.plan().map(|_| ())
    }
}

/// Build a PBR material and wire its texture maps in.
pub async fn pbr_material(executor: &dyn Executor, request: PbrRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.material.pbr", executor);

    let plan = match request.spec.plan() {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the shader graph", error);
            return run.finish(false).await;
        }
    };
    run.note(
        "plan the shader graph",
        json!({
            "nodes": plan.nodes.len(),
            "links": plan.links.len(),
            "images": request.spec.required_images()
                .into_iter()
                .map(|(image, colorspace)| json!({"image": image, "colorspace": colorspace}))
                .collect::<Vec<_>>(),
        }),
    );

    let created = run
        .step(
            "create the material",
            "material.create",
            json!({"name": request.name, "use_nodes": true}),
        )
        .await;
    let Some(created) = created else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(material_id) = run.created_material("material", &created) else {
        run.fail(
            "read the new material id",
            BlenderError::internal("material.create returned no id"),
        );
        return run.finish(request.rollback_on_failure).await;
    };

    run.step(
        "build the shader graph",
        "shader.graph.build",
        json!({
            "material": material_id,
            "clear": true,
            "nodes": plan.nodes,
            "links": plan.links,
        }),
    )
    .await;

    if !request.assign_to.is_empty() {
        run.step(
            "assign the material",
            "material.assign",
            json!({"material": material_id, "objects": request.assign_to}),
        )
        .await;
    }

    run.finish(request.rollback_on_failure).await
}

/// `workflow.material.glass`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GlassRequest {
    pub name: String,
    #[serde(flatten)]
    pub spec: GlassSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assign_to: Vec<ObjectRef>,
    #[serde(default = "default_true")]
    pub rollback_on_failure: bool,
}

impl blender_protocol::Validate for GlassRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        blender_protocol::check_name(&self.name, "name")?;
        self.spec.plan().map(|_| ())
    }
}

/// Build a glass material.
pub async fn glass_material(executor: &dyn Executor, request: GlassRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.material.glass", executor);

    let plan = match request.spec.plan() {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the shader graph", error);
            return run.finish(false).await;
        }
    };
    run.note("plan the shader graph", json!({"nodes": plan.nodes.len()}));

    let Some(created) = run
        .step(
            "create the material",
            "material.create",
            json!({"name": request.name, "use_nodes": true}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(material_id) = run.created_material("material", &created) else {
        run.fail("read the new material id", BlenderError::internal("no id"));
        return run.finish(request.rollback_on_failure).await;
    };

    run.step(
        "build the shader graph",
        "shader.graph.build",
        json!({"material": material_id, "clear": true, "nodes": plan.nodes, "links": plan.links}),
    )
    .await;

    // Glass needs blended alpha in EEVEE or it renders as an opaque lump.
    run.optional_step(
        "set the blend mode for EEVEE",
        "material.update",
        json!({"material": material_id, "settings": {"blend_method": "BLEND"}}),
    )
    .await;

    if !request.assign_to.is_empty() {
        run.step(
            "assign the material",
            "material.assign",
            json!({"material": material_id, "objects": request.assign_to}),
        )
        .await;
    }

    run.finish(request.rollback_on_failure).await
}

/// `workflow.material.emissive`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmissiveRequest {
    pub name: String,
    #[serde(flatten)]
    pub spec: EmissiveSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assign_to: Vec<ObjectRef>,
    #[serde(default = "default_true")]
    pub rollback_on_failure: bool,
}

impl blender_protocol::Validate for EmissiveRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        blender_protocol::check_name(&self.name, "name")?;
        self.spec.plan().map(|_| ())
    }
}

/// Build an emissive material.
pub async fn emissive_material(
    executor: &dyn Executor,
    request: EmissiveRequest,
) -> WorkflowReport {
    let mut run = Run::new("workflow.material.emissive", executor);

    let plan = match request.spec.plan() {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the shader graph", error);
            return run.finish(false).await;
        }
    };
    run.note("plan the shader graph", json!({"nodes": plan.nodes.len()}));

    let Some(created) = run
        .step(
            "create the material",
            "material.create",
            json!({"name": request.name, "use_nodes": true}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(material_id) = run.created_material("material", &created) else {
        run.fail("read the new material id", BlenderError::internal("no id"));
        return run.finish(request.rollback_on_failure).await;
    };

    run.step(
        "build the shader graph",
        "shader.graph.build",
        json!({"material": material_id, "clear": true, "nodes": plan.nodes, "links": plan.links}),
    )
    .await;

    if !request.assign_to.is_empty() {
        run.step(
            "assign the material",
            "material.assign",
            json!({"material": material_id, "objects": request.assign_to}),
        )
        .await;
    }

    run.finish(request.rollback_on_failure).await
}

/// Pull the material id out of a workflow report, for callers chaining steps.
pub fn material_id(report: &WorkflowReport) -> Option<&str> {
    report
        .created
        .get("material")
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use blender_domain::material::{MapKind, TextureMap};
    use blender_protocol::math::Color4;

    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn material_result() -> Value {
        json!({"material": {"id": "mat-1", "name": "Concrete"}})
    }

    fn request(maps: Vec<TextureMap>) -> PbrRequest {
        PbrRequest {
            name: "Concrete".into(),
            spec: PbrSpec {
                maps,
                base_color: Some(Color4::rgb(0.5, 0.5, 0.5)),
                roughness: Some(0.8),
                metallic: Some(0.0),
                uv_scale: None,
                normal_strength: None,
                displacement_scale: None,
            },
            assign_to: vec![],
            rollback_on_failure: true,
        }
    }

    #[tokio::test]
    async fn a_pbr_material_is_four_operations_at_most() {
        let recorder = Recorder::new(material_result());
        let mut request = request(vec![
            TextureMap {
                kind: MapKind::BaseColor,
                image: "c.png".into(),
            },
            TextureMap {
                kind: MapKind::Roughness,
                image: "r.png".into(),
            },
            TextureMap {
                kind: MapKind::Normal,
                image: "n.png".into(),
            },
        ]);
        request.assign_to = vec![ObjectRef::name("Wall")];

        let report = pbr_material(&recorder, request).await;
        assert!(report.success, "{report:?}");
        assert_eq!(
            recorder.ops(),
            ["material.create", "shader.graph.build", "material.assign"],
            "the whole graph goes in one build, however many maps there are"
        );
    }

    #[tokio::test]
    async fn the_graph_is_planned_before_anything_is_created() {
        let recorder = Recorder::new(material_result());
        let report = pbr_material(&recorder, request(vec![])).await;
        // The first step does work in Rust and touches nothing.
        assert_eq!(report.steps[0].name, "plan the shader graph");
        assert!(report.steps[0].op.is_none());
    }

    #[tokio::test]
    async fn an_unplannable_material_never_reaches_blender() {
        let recorder = Recorder::new(material_result());
        let request = request(vec![
            TextureMap {
                kind: MapKind::Normal,
                image: "a.png".into(),
            },
            TextureMap {
                kind: MapKind::Normal,
                image: "b.png".into(),
            },
        ]);
        let report = pbr_material(&recorder, request).await;
        assert!(!report.success);
        assert!(recorder.ops().is_empty(), "nothing should have been sent");
    }

    #[tokio::test]
    async fn a_failed_graph_build_removes_the_material() {
        let recorder = Recorder::new(material_result()).expect(
            "shader.graph.build",
            Reply::Fail(BlenderError::new(
                blender_protocol::ErrorCode::InvalidNodeType,
                "no such node",
            )),
        );
        let report = pbr_material(&recorder, request(vec![])).await;
        assert!(!report.success);
        assert!(
            recorder.called("material.delete"),
            "the half-built material must not survive"
        );
        assert!(report.rollback.unwrap().complete);
    }

    #[tokio::test]
    async fn rollback_can_be_turned_off() {
        let recorder = Recorder::new(material_result()).expect(
            "shader.graph.build",
            Reply::Fail(BlenderError::invalid_argument("no")),
        );
        let mut request = request(vec![]);
        request.rollback_on_failure = false;
        let report = pbr_material(&recorder, request).await;
        assert!(!report.success);
        assert!(!recorder.called("material.delete"));
        assert_eq!(
            material_id(&report),
            Some("mat-1"),
            "the material is still there"
        );
    }

    #[tokio::test]
    async fn glass_sets_the_eevee_blend_mode() {
        let recorder = Recorder::new(material_result());
        let report = glass_material(
            &recorder,
            GlassRequest {
                name: "Glass".into(),
                spec: GlassSpec {
                    ior: 1.45,
                    roughness: 0.0,
                    color: None,
                    use_glass_bsdf: false,
                },
                assign_to: vec![],
                rollback_on_failure: true,
            },
        )
        .await;
        assert!(report.success);
        assert!(recorder.called("material.update"));
    }

    #[tokio::test]
    async fn a_failed_blend_mode_does_not_fail_the_glass_workflow() {
        // The blend mode is cosmetic in Cycles, so its failure must not throw
        // away a material that is otherwise fine.
        let recorder = Recorder::new(material_result()).expect(
            "material.update",
            Reply::Fail(BlenderError::new(
                blender_protocol::ErrorCode::UnsupportedProperty,
                "no blend_method",
            )),
        );
        let report = glass_material(
            &recorder,
            GlassRequest {
                name: "Glass".into(),
                spec: GlassSpec {
                    ior: 1.5,
                    roughness: 0.0,
                    color: None,
                    use_glass_bsdf: true,
                },
                assign_to: vec![],
                rollback_on_failure: true,
            },
        )
        .await;
        assert!(report.success);
        assert!(!recorder.called("material.delete"));
    }

    #[tokio::test]
    async fn emission_builds_and_assigns() {
        let recorder = Recorder::new(material_result());
        let report = emissive_material(
            &recorder,
            EmissiveRequest {
                name: "Neon".into(),
                spec: EmissiveSpec {
                    color: Color4::rgb(0.1, 0.9, 1.0),
                    strength: 12.0,
                    pure: true,
                },
                assign_to: vec![ObjectRef::name("Sign")],
                rollback_on_failure: true,
            },
        )
        .await;
        assert!(report.success);
        assert!(recorder.called("material.assign"));
    }
}

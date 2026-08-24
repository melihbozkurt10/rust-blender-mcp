//! Workflow tools.
//!
//! Each of these runs a multi-step workflow from the `workflow-engine` crate.
//! The engine reaches Blender through the tool registry rather than the raw
//! transport, so every step inside a workflow gets the same validation,
//! capability checks and managed-path handling as a direct call would.

use std::sync::Arc;

use blender_protocol::{
    BlenderError,
    command::{Category, OpKind},
};
use serde_json::Value;
use workflow_engine::{
    Executor,
    executor::BoxFuture,
    workflows::{
        export::PrepareRequest,
        geometry::{ArrayRequest, ScatterRequest},
        lighting::ThreePointRequest,
        material::{EmissiveRequest, GlassRequest, PbrRequest},
        modelling::{WallRequest, WallRunRequest},
        render::{StudioRequest, TurntableRequest},
    },
};

use crate::{registry::ToolSpec, state::AppState};

const WORKFLOWS: Category = Category::Workflows;

/// Lets a workflow call the server's own tools.
///
/// Routing through the registry rather than the transport is deliberate: a
/// workflow that renders should get the managed output path and the artifact
/// bookkeeping, not a raw `render.execute` that writes wherever it likes.
pub struct ToolExecutor {
    state: Arc<AppState>,
}

impl ToolExecutor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl Executor for ToolExecutor {
    fn call<'a>(&'a self, op: &'a str, args: Value) -> BoxFuture<'a, Result<Value, BlenderError>> {
        let state = Arc::clone(&self.state);
        let op = op.to_string();
        Box::pin(async move {
            match state.registry.get(&op) {
                Some(spec) => {
                    let handler = Arc::clone(&spec.handler);
                    handler(Arc::clone(&state), args).await
                }
                // Operations the bridge has but the MCP surface does not expose
                // as a tool, such as the graph builders workflows target.
                None => state.client.call(&op, args).await,
            }
        })
    }
}

/// Turn a workflow report into a tool result, failing the call when the
/// workflow failed so the model sees an error rather than a success with a
/// buried `success: false`.
fn finish(report: workflow_engine::run::WorkflowReport) -> Result<Value, BlenderError> {
    let value =
        serde_json::to_value(&report).map_err(|error| BlenderError::internal(error.to_string()))?;
    if report.success {
        return Ok(value);
    }
    let mut error = report
        .error
        .as_ref()
        .map(|step| {
            BlenderError::new(
                blender_protocol::ErrorCode::UnsupportedOperation,
                step.message.clone(),
            )
        })
        .unwrap_or_else(|| BlenderError::internal("the workflow failed"));
    error = error.with_detail_json("report", &value);
    if let Some(step) = report.error.as_ref() {
        error = error.with_detail("failed_step_code", step.code.clone());
    }
    Err(error)
}

macro_rules! workflow_tool {
    ($name:literal, $params:ty, $title:literal, $description:literal, $body:path) => {
        ToolSpec::custom::<$params, _, _>(
            $name,
            WORKFLOWS,
            OpKind::Write,
            $title,
            $description,
            |state: Arc<AppState>, params: $params| async move {
                let executor = ToolExecutor::new(state);
                finish($body(&executor, params).await)
            },
        )
    };
}

pub fn tools() -> Vec<ToolSpec> {
    vec![
        workflow_tool!(
            "workflow.material.pbr",
            PbrRequest,
            "Build a PBR material",
            "Create a material and wire a full PBR graph from a set of texture maps: base colour, \
             roughness, metallic, normal, height, ambient occlusion, emission and alpha. The graph \
             is planned server-side and sent in one call, normal maps go through a normal map \
             node, height drives displacement, and ambient occlusion multiplies the base colour. \
             Load the images first with `image.load`, setting data maps to `Non-Color`.",
            workflow_engine::workflows::pbr_material
        ),
        workflow_tool!(
            "workflow.material.glass",
            GlassRequest,
            "Build a glass material",
            "Create glass with a matched IOR and roughness, using either a Principled BSDF with \
             transmission (better in EEVEE) or a dedicated Glass BSDF (more direct in Cycles). \
             The EEVEE blend mode is set for you.",
            workflow_engine::workflows::glass_material
        ),
        workflow_tool!(
            "workflow.material.emissive",
            EmissiveRequest,
            "Build an emissive material",
            "Create a light-emitting material, either a pure Emission shader or a Principled BSDF \
             with emission, and assign it.",
            workflow_engine::workflows::emissive_material
        ),
        workflow_tool!(
            "workflow.lighting.three_point",
            ThreePointRequest,
            "Build a three-point lighting rig",
            "Create key, fill and rim lights sized and placed from the subject's own bounds, with \
             the conventional intensity ratios and colour temperatures. The fill has its specular \
             contribution reduced, because a fill that adds highlights stops being a fill.",
            workflow_engine::workflows::three_point
        ),
        workflow_tool!(
            "workflow.render.studio",
            StudioRequest,
            "Set up a studio shot",
            "Frame a camera on the subject, build a three-point rig, set the world and apply \
             render settings. The camera distance is computed from the subject's bounds and the \
             lens, not found by trial and error.",
            workflow_engine::workflows::studio_render
        ),
        workflow_tool!(
            "workflow.product_turntable",
            TurntableRequest,
            "Build a product turntable",
            "Set up a studio shot, create an empty at the subject's centre, parent the camera to \
             it and animate a full turn. The subject itself is never rotated -- spinning it would \
             change the scene in ways nobody asked for and break anything parented to it.",
            workflow_engine::workflows::product_turntable
        ),
        workflow_tool!(
            "workflow.model.create_wall",
            WallRequest,
            "Build a wall",
            "Create a wall between two points with a given height and thickness. The position, \
             rotation and size are computed from the two points; a height difference between the \
             ends is ignored rather than producing a leaning wall.",
            workflow_engine::workflows::create_wall
        ),
        workflow_tool!(
            "workflow.model.create_wall_run",
            WallRunRequest,
            "Build a run of walls",
            "Create a wall for each segment of a polyline, optionally closing it into a room.",
            workflow_engine::workflows::modelling::create_wall_run
        ),
        workflow_tool!(
            "workflow.export.prepare",
            PrepareRequest,
            "Prepare for export",
            "Check the scene against a target profile -- generic, game asset, glTF, Unreal, Unity \
             -- and report what would break on import: unapplied scale, missing UVs, \
             non-manifold geometry, missing textures, too many bone influences, unsafe names. \
             Defaults to a dry run, and can apply the fixes and export once it passes.",
            workflow_engine::workflows::prepare_export
        ),
        workflow_tool!(
            "geometry_nodes.scatter",
            ScatterRequest,
            "Scatter objects over a surface",
            "Build and attach a geometry node graph that distributes instances over a mesh, with \
             density, seed, scale range, rotation jitter, normal alignment and an optional \
             minimum distance for a Poisson-disk distribution that does not clump.",
            workflow_engine::workflows::scatter
        ),
        workflow_tool!(
            "geometry_nodes.array_along_curve",
            ArrayRequest,
            "Array objects along a curve",
            "Build and attach a geometry node graph that places copies of an object along a \
             curve, by count or by spacing, optionally rotating them to follow the curve.",
            workflow_engine::workflows::array_along_curve
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_workflow_is_in_the_workflow_category() {
        for tool in tools() {
            assert_eq!(tool.category, WORKFLOWS, "{}", tool.name);
            assert_eq!(tool.kind, OpKind::Write, "{}", tool.name);
        }
    }

    #[test]
    fn workflow_descriptions_explain_the_reasoning() {
        // A workflow that does non-obvious work should say what it decided, or
        // a caller cannot tell whether to use it or do it by hand.
        let turntable = tools()
            .into_iter()
            .find(|t| t.name == "workflow.product_turntable")
            .unwrap();
        assert!(turntable.description.contains("never rotated"));

        let pbr = tools()
            .into_iter()
            .find(|t| t.name == "workflow.material.pbr")
            .unwrap();
        assert!(pbr.description.contains("Non-Color"));
    }

    #[test]
    fn a_failed_workflow_becomes_an_error_result() {
        let report = workflow_engine::run::WorkflowReport {
            workflow: "test".into(),
            success: false,
            steps: vec![],
            created: Default::default(),
            error: Some(workflow_engine::step::StepError {
                code: "OBJECT_NOT_FOUND".into(),
                message: "no such object".into(),
                details: Default::default(),
            }),
            rollback: None,
        };
        let error = finish(report).unwrap_err();
        assert!(error.message.contains("no such object"));
        assert_eq!(error.details["failed_step_code"], "OBJECT_NOT_FOUND");
        assert!(
            error.details.contains_key("report"),
            "the full report must survive"
        );
    }

    #[test]
    fn a_successful_workflow_returns_its_report() {
        let report = workflow_engine::run::WorkflowReport {
            workflow: "test".into(),
            success: true,
            steps: vec![],
            created: Default::default(),
            error: None,
            rollback: None,
        };
        let value = finish(report).unwrap();
        assert_eq!(value["success"], true);
    }
}

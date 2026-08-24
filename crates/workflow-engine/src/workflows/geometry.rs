//! Geometry node workflows.
//!
//! Scatter and array-along-curve are both: plan the graph in Rust, create a
//! node group, send the graph as one build, attach the modifier. Four calls,
//! whatever the graph's size.

use blender_domain::graph::{plan_array_along_curve, plan_scatter};
use blender_protocol::{
    Validate,
    geometry_nodes::{ArrayAlongCurve, Scatter},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    rollback::Compensation,
    run::{Run, WorkflowReport},
};

/// `geometry_nodes.scatter`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScatterRequest {
    #[serde(flatten)]
    pub scatter: Scatter,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl Validate for ScatterRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        plan_scatter(&self.scatter).map(|_| ())
    }
}

/// Scatter instances over a surface.
pub async fn scatter(executor: &dyn Executor, request: ScatterRequest) -> WorkflowReport {
    let mut run = Run::new("geometry_nodes.scatter", executor);

    let plan = match plan_scatter(&request.scatter) {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the scatter graph", error);
            return run.finish(false).await;
        }
    };
    run.note(
        "plan the scatter graph",
        json!({"nodes": plan.nodes.len(), "links": plan.links.len()}),
    );

    let group_name = request
        .scatter
        .name
        .clone()
        .unwrap_or_else(|| "Scatter".to_string());

    let Some(created) = run
        .step(
            "create the node group",
            "geometry_nodes.group.create",
            json!({"name": group_name, "with_geometry_io": true}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(group_id) = created
        .get("group")
        .and_then(|g| g.get("id"))
        .and_then(Value::as_str)
    else {
        run.fail(
            "read the new group id",
            blender_protocol::BlenderError::internal("group.create returned no id"),
        );
        return run.finish(request.rollback_on_failure).await;
    };
    let group_id = group_id.to_string();
    run.created(
        "group",
        created.get("group").cloned().unwrap_or(Value::Null),
    );
    run.compensate(Compensation::delete_node_group(&group_id));

    run.step(
        "build the scatter graph",
        "geometry_nodes.graph.build",
        json!({
            "node_tree": group_id,
            "clear": true,
            "nodes": plan.nodes,
            "links": plan.links,
        }),
    )
    .await;

    let Some(attached) = run
        .step(
            "attach it to the surface",
            "geometry_nodes.modifier.attach",
            json!({"object": request.scatter.surface, "group": group_id, "modifier_name": group_name}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    if let Some(modifier) = attached.get("modifier").and_then(Value::as_str) {
        run.created("modifier", json!({"name": modifier}));
        run.compensate(Compensation::remove_modifier(
            request.scatter.surface.to_string(),
            modifier,
        ));
    }

    run.finish(request.rollback_on_failure).await
}

/// `geometry_nodes.array_along_curve`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArrayRequest {
    #[serde(flatten)]
    pub array: ArrayAlongCurve,
    /// Object to attach the modifier to. Defaults to the curve, so the array
    /// follows it if the curve is edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<blender_protocol::ids::ObjectRef>,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl Validate for ArrayRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        plan_array_along_curve(&self.array).map(|_| ())
    }
}

/// Array copies of an object along a curve.
pub async fn array_along_curve(executor: &dyn Executor, request: ArrayRequest) -> WorkflowReport {
    let mut run = Run::new("geometry_nodes.array_along_curve", executor);

    let plan = match plan_array_along_curve(&request.array) {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the array graph", error);
            return run.finish(false).await;
        }
    };
    run.note(
        "plan the array graph",
        json!({"nodes": plan.nodes.len(), "links": plan.links.len()}),
    );

    let group_name = request
        .array
        .name
        .clone()
        .unwrap_or_else(|| "ArrayAlongCurve".to_string());
    let target = request
        .attach_to
        .clone()
        .unwrap_or_else(|| request.array.curve.clone());

    let Some(created) = run
        .step(
            "create the node group",
            "geometry_nodes.group.create",
            json!({"name": group_name, "with_geometry_io": true}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(group_id) = created
        .get("group")
        .and_then(|g| g.get("id"))
        .and_then(Value::as_str)
    else {
        run.fail(
            "read the new group id",
            blender_protocol::BlenderError::internal("group.create returned no id"),
        );
        return run.finish(request.rollback_on_failure).await;
    };
    let group_id = group_id.to_string();
    run.created(
        "group",
        created.get("group").cloned().unwrap_or(Value::Null),
    );
    run.compensate(Compensation::delete_node_group(&group_id));

    run.step(
        "build the array graph",
        "geometry_nodes.graph.build",
        json!({
            "node_tree": group_id,
            "clear": true,
            "nodes": plan.nodes,
            "links": plan.links,
        }),
    )
    .await;

    let Some(attached) = run
        .step(
            "attach it",
            "geometry_nodes.modifier.attach",
            json!({"object": target, "group": group_id, "modifier_name": group_name}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    if let Some(modifier) = attached.get("modifier").and_then(Value::as_str) {
        run.created("modifier", json!({"name": modifier}));
        run.compensate(Compensation::remove_modifier(target.to_string(), modifier));
    }

    run.finish(request.rollback_on_failure).await
}

#[cfg(test)]
mod tests {
    use blender_protocol::{
        geometry_nodes::{CurveSpacing, ScatterSource},
        ids::ObjectRef,
        math::Axis,
    };

    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn group_result() -> Value {
        json!({"group": {"id": "grp-1", "name": "Scatter"}})
    }

    fn scatter_request() -> ScatterRequest {
        ScatterRequest {
            scatter: Scatter {
                surface: ObjectRef::name("Ground"),
                source: ScatterSource::Object(ObjectRef::name("Rock")),
                density: 5.0,
                seed: 1,
                scale_min: 0.8,
                scale_max: 1.4,
                rotation_jitter: None,
                align_to_normal: true,
                density_attribute: None,
                minimum_distance: None,
                realize_instances: false,
                name: Some("RockScatter".into()),
            },
            rollback_on_failure: true,
        }
    }

    #[tokio::test]
    async fn a_scatter_is_three_calls_whatever_the_graph_size() {
        let recorder = Recorder::new(json!({"modifier": "RockScatter"}))
            .expect("geometry_nodes.group.create", Reply::Ok(group_result()));
        let report = scatter(&recorder, scatter_request()).await;
        assert!(report.success, "{report:?}");
        assert_eq!(
            recorder.ops(),
            [
                "geometry_nodes.group.create",
                "geometry_nodes.graph.build",
                "geometry_nodes.modifier.attach",
            ]
        );
    }

    #[tokio::test]
    async fn the_graph_goes_over_as_one_plan() {
        let recorder = Recorder::new(json!({"modifier": "RockScatter"}))
            .expect("geometry_nodes.group.create", Reply::Ok(group_result()));
        scatter(&recorder, scatter_request()).await;
        let build = recorder.args_for("geometry_nodes.graph.build").unwrap();
        assert!(build["nodes"].as_array().unwrap().len() >= 4);
        assert!(build["links"].as_array().unwrap().len() >= 3);
        assert_eq!(build["clear"], true);
    }

    #[tokio::test]
    async fn a_failed_attach_removes_the_group() {
        let recorder = Recorder::new(json!({}))
            .expect("geometry_nodes.group.create", Reply::Ok(group_result()))
            .expect("geometry_nodes.graph.build", Reply::Ok(json!({})))
            .expect(
                "geometry_nodes.modifier.attach",
                Reply::Fail(blender_protocol::BlenderError::not_found(
                    "object", "Ground",
                )),
            );
        let report = scatter(&recorder, scatter_request()).await;
        assert!(!report.success);
        assert!(recorder.called("geometry_nodes.group.delete"));
    }

    #[tokio::test]
    async fn an_unplannable_scatter_never_reaches_blender() {
        let recorder = Recorder::new(json!({}));
        let mut request = scatter_request();
        request.scatter.scale_min = 4.0;
        request.scatter.scale_max = 1.0;
        let report = scatter(&recorder, request).await;
        assert!(!report.success);
        assert!(recorder.ops().is_empty());
    }

    #[tokio::test]
    async fn an_array_attaches_to_the_curve_by_default() {
        let recorder = Recorder::new(json!({"modifier": "Fence"}))
            .expect("geometry_nodes.group.create", Reply::Ok(group_result()));
        let report = array_along_curve(
            &recorder,
            ArrayRequest {
                array: ArrayAlongCurve {
                    source: ObjectRef::name("Post"),
                    curve: ObjectRef::name("Path"),
                    spacing: CurveSpacing::Spacing(2.0),
                    align_axis: Axis::Y,
                    offset: None,
                    follow_curve: true,
                    realize_instances: false,
                    name: Some("Fence".into()),
                },
                attach_to: None,
                rollback_on_failure: true,
            },
        )
        .await;

        assert!(report.success, "{report:?}");
        let attach = recorder.args_for("geometry_nodes.modifier.attach").unwrap();
        assert_eq!(attach["object"], "Path");
    }
}

//! Modelling workflows.

use blender_domain::modeling::{WallRun, WallSpec};
use blender_protocol::{Validate, ids::CollectionRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    run::{Run, WorkflowReport},
};

/// `workflow.model.create_wall`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WallRequest {
    #[serde(flatten)]
    pub wall: WallSpec,
    /// Collection to put the wall in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    /// Material to assign once it is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl Validate for WallRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        // Planning is the validation: a wall that cannot be placed cannot be
        // built, and the reason comes back before Blender is touched.
        self.wall.plan().map(|_| ())
    }
}

/// Build one wall between two points.
pub async fn create_wall(executor: &dyn Executor, request: WallRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.model.create_wall", executor);

    let placement = match request.wall.plan() {
        Ok(placement) => placement,
        Err(error) => {
            run.fail("work out where the wall goes", error);
            return run.finish(false).await;
        }
    };
    run.note(
        "work out where the wall goes",
        json!({
            "location": placement.location,
            "rotation_euler": placement.rotation_euler,
            "dimensions": placement.dimensions,
            "length": placement.length,
        }),
    );

    let mut args = json!({
        "type": "CUBE",
        "name": request.wall.name.clone().unwrap_or_else(|| "Wall".to_string()),
        "location": placement.location,
        "rotation": {"euler": placement.rotation_euler},
        "dimensions": placement.dimensions,
    });
    if let Some(collection) = &request.collection
        && let Value::Object(map) = &mut args
    {
        map.insert("collection".into(), json!(collection));
    }

    let Some(created) = run.step("create the wall", "object.create", args).await else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(object_id) = run.created_object("object", &created) else {
        run.fail(
            "read the new object id",
            blender_protocol::BlenderError::internal("object.create returned no id"),
        );
        return run.finish(request.rollback_on_failure).await;
    };

    if let Some(material) = &request.material {
        run.step(
            "assign the material",
            "material.assign",
            json!({"material": material, "objects": [object_id]}),
        )
        .await;
    }

    run.finish(request.rollback_on_failure).await
}

/// `workflow.model.create_wall_run`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WallRunRequest {
    #[serde(flatten)]
    pub run: WallRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl Validate for WallRunRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        self.run.plan().map(|_| ())
    }
}

/// Build a run of walls from a list of corner points.
pub async fn create_wall_run(executor: &dyn Executor, request: WallRunRequest) -> WorkflowReport {
    let mut workflow = Run::new("workflow.model.create_wall_run", executor);

    let placements = match request.run.plan() {
        Ok(placements) => placements,
        Err(error) => {
            workflow.fail("work out the wall run", error);
            return workflow.finish(false).await;
        }
    };
    workflow.note(
        "work out the wall run",
        json!({
            "segments": placements.len(),
            "total_length": placements.iter().map(|p| p.length).sum::<f64>(),
        }),
    );

    let prefix = request
        .name_prefix
        .clone()
        .unwrap_or_else(|| "Wall".to_string());
    let mut ids = Vec::new();
    for (index, placement) in placements.iter().enumerate() {
        let mut args = json!({
            "type": "CUBE",
            "name": format!("{prefix}_{:02}", index + 1),
            "location": placement.location,
            "rotation": {"euler": placement.rotation_euler},
            "dimensions": placement.dimensions,
        });
        if let Some(collection) = &request.collection
            && let Value::Object(map) = &mut args
        {
            map.insert("collection".into(), json!(collection));
        }

        let Some(created) = workflow
            .step(
                &format!("create wall segment {}", index + 1),
                "object.create",
                args,
            )
            .await
        else {
            return workflow.finish(request.rollback_on_failure).await;
        };
        if let Some(id) = created
            .get("object")
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
        {
            ids.push(id.to_string());
            workflow.compensate(crate::rollback::Compensation::delete_object(id));
        }
    }

    workflow.created("walls", json!(ids));

    if let Some(material) = &request.material
        && !ids.is_empty()
    {
        workflow
            .step(
                "assign the material",
                "material.assign",
                json!({"material": material, "objects": ids}),
            )
            .await;
    }

    workflow.finish(request.rollback_on_failure).await
}

#[cfg(test)]
mod tests {
    use blender_protocol::math::Vec3;

    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn wall_result(id: &str) -> Value {
        json!({"object": {"id": id, "name": "Wall"}})
    }

    fn request() -> WallRequest {
        WallRequest {
            wall: WallSpec {
                start: Vec3::ZERO,
                end: Vec3::new(5.0, 0.0, 0.0),
                height: 3.0,
                thickness: 0.2,
                name: Some("Wall".into()),
                centred_vertically: false,
            },
            collection: None,
            material: None,
            rollback_on_failure: true,
        }
    }

    #[tokio::test]
    async fn a_wall_is_one_create_with_computed_geometry() {
        let recorder = Recorder::new(wall_result("w1"));
        let report = create_wall(&recorder, request()).await;
        assert!(report.success, "{report:?}");
        assert_eq!(recorder.ops(), ["object.create"]);

        let args = recorder.args_for("object.create").unwrap();
        assert_eq!(args["dimensions"]["x"], 5.0);
        assert_eq!(args["dimensions"]["z"], 3.0);
        assert_eq!(args["location"]["x"], 2.5);
        assert_eq!(args["location"]["z"], 1.5);
    }

    #[tokio::test]
    async fn the_geometry_is_worked_out_before_anything_is_created() {
        let recorder = Recorder::new(wall_result("w1"));
        let report = create_wall(&recorder, request()).await;
        assert_eq!(report.steps[0].name, "work out where the wall goes");
        assert!(
            report.steps[0].op.is_none(),
            "no Blender call for the maths"
        );
    }

    #[tokio::test]
    async fn an_impossible_wall_never_reaches_blender() {
        let recorder = Recorder::new(wall_result("w1"));
        let mut request = request();
        request.wall.end = request.wall.start;
        let report = create_wall(&recorder, request).await;
        assert!(!report.success);
        assert!(recorder.ops().is_empty());
    }

    #[tokio::test]
    async fn a_failed_material_assignment_removes_the_wall() {
        let recorder = Recorder::new(wall_result("w1")).expect(
            "material.assign",
            Reply::Fail(blender_protocol::BlenderError::not_found(
                "material", "Brick",
            )),
        );
        let mut request = request();
        request.material = Some("Brick".into());
        let report = create_wall(&recorder, request).await;
        assert!(!report.success);
        assert!(recorder.called("object.delete"));
    }

    #[tokio::test]
    async fn a_closed_run_makes_one_wall_per_edge() {
        let recorder = Recorder::new(wall_result("w"));
        let report = create_wall_run(
            &recorder,
            WallRunRequest {
                run: WallRun {
                    points: vec![
                        Vec3::ZERO,
                        Vec3::new(4.0, 0.0, 0.0),
                        Vec3::new(4.0, 3.0, 0.0),
                        Vec3::new(0.0, 3.0, 0.0),
                    ],
                    height: 2.5,
                    thickness: 0.15,
                    closed: true,
                },
                name_prefix: Some("Room".into()),
                collection: None,
                material: None,
                rollback_on_failure: true,
            },
        )
        .await;

        assert!(report.success, "{report:?}");
        let creates = recorder
            .ops()
            .into_iter()
            .filter(|o| o == "object.create")
            .count();
        assert_eq!(creates, 4);
        assert_eq!(report.created["walls"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_run_that_fails_midway_removes_everything_it_made() {
        let recorder = Recorder::new(wall_result("w"))
            .expect("object.create", Reply::Ok(wall_result("a")))
            .expect("object.create", Reply::Ok(wall_result("b")))
            .expect(
                "object.create",
                Reply::Fail(blender_protocol::BlenderError::invalid_argument("no")),
            );
        let report = create_wall_run(
            &recorder,
            WallRunRequest {
                run: WallRun {
                    points: vec![
                        Vec3::ZERO,
                        Vec3::new(4.0, 0.0, 0.0),
                        Vec3::new(4.0, 3.0, 0.0),
                        Vec3::new(0.0, 3.0, 0.0),
                    ],
                    height: 2.5,
                    thickness: 0.15,
                    closed: false,
                },
                name_prefix: None,
                collection: None,
                material: None,
                rollback_on_failure: true,
            },
        )
        .await;

        assert!(!report.success);
        let deletes = recorder
            .ops()
            .into_iter()
            .filter(|o| o == "object.delete")
            .count();
        assert_eq!(deletes, 2, "both completed walls should be removed");
    }
}

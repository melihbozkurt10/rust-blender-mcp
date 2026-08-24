//! Lighting workflows.

use blender_domain::lighting::ThreePointSpec;
use blender_protocol::{ids::ObjectRef, math::Aabb};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    run::{Run, WorkflowReport},
};

/// `workflow.lighting.three_point`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreePointRequest {
    /// What the rig lights. Its bounds decide where the lights stand and how
    /// bright they are.
    pub target: ObjectRef,
    /// Key light power in watts. Derived from the subject size when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_energy: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rim_ratio: Option<f64>,
    /// How far the lights stand, as a multiple of the subject radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_temperature: Option<f64>,
    /// Collection to put the lights in, so they can be hidden as a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Prefix for the light names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_prefix: Option<String>,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl blender_protocol::Validate for ThreePointRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        for (value, field) in [
            (self.fill_ratio, "fill_ratio"),
            (self.rim_ratio, "rim_ratio"),
        ] {
            if let Some(value) = value {
                blender_protocol::math::check_range(value, 0.0, 4.0, field)?;
            }
        }
        if let Some(factor) = self.distance_factor {
            blender_protocol::math::check_range(factor, 0.5, 100.0, "distance_factor")?;
        }
        if let Some(temperature) = self.key_temperature {
            blender_protocol::math::check_range(temperature, 1000.0, 40000.0, "key_temperature")?;
        }
        Ok(())
    }
}

/// Build a three-point rig around a subject.
pub async fn three_point(executor: &dyn Executor, request: ThreePointRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.lighting.three_point", executor);

    // The subject's bounds drive everything, so they are read first.
    let Some(analysis) = run
        .step(
            "measure the subject",
            "object.get",
            json!({"object": request.target}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };

    let bounds = match bounds_from_object(&analysis) {
        Some(bounds) => bounds,
        None => {
            run.fail(
                "measure the subject",
                blender_protocol::BlenderError::invalid_argument(
                    "The target reported no size, so there is nothing to light. Empties and \
                     cameras have no bounds; point the rig at geometry.",
                ),
            );
            return run.finish(request.rollback_on_failure).await;
        }
    };

    let mut spec = ThreePointSpec {
        subject: bounds,
        key_energy: request.key_energy,
        fill_ratio: request.fill_ratio.unwrap_or(0.35),
        rim_ratio: request.rim_ratio.unwrap_or(0.6),
        distance_factor: request.distance_factor.unwrap_or(4.0),
        key_temperature: request.key_temperature.unwrap_or(5200.0),
        fill_temperature: 6500.0,
        rim_temperature: 6200.0,
        facing: blender_protocol::math::Vec3::new(0.0, -1.0, 0.0),
    };
    // A cool key should not end up with a warmer fill than itself: the
    // convention is a warm key against a cooler fill, and inverting it looks
    // wrong in a way that is hard to diagnose later.
    if spec.key_temperature > spec.fill_temperature {
        spec.fill_temperature = spec.key_temperature + 1300.0;
        spec.rim_temperature = spec.key_temperature + 1000.0;
    }

    let plan = match spec.plan() {
        Ok(plan) => plan,
        Err(error) => {
            run.fail("plan the rig", error);
            return run.finish(request.rollback_on_failure).await;
        }
    };
    run.note(
        "plan the rig",
        json!({
            "target": plan.target,
            "radius": plan.radius,
            "distance": plan.distance,
            "lights": plan.lights.len(),
        }),
    );

    let mut collection_id = request.collection.clone();
    if collection_id.is_none()
        && let Some(created) = run
            .optional_step(
                "create a collection for the lights",
                "collection.create",
                json!({"name": format!("{}Lighting", request.name_prefix.clone().unwrap_or_default())}),
            )
            .await
        && let Some(id) = created.get("collection").and_then(|c| c.get("id")).and_then(Value::as_str)
    {
        collection_id = Some(id.to_string());
        run.compensate(crate::rollback::Compensation::delete_collection(id));
    }

    let prefix = request.name_prefix.clone().unwrap_or_default();
    for light in &plan.lights {
        let mut args = json!({
            "type": light.light_type,
            "name": format!("{prefix}{}", light.name),
            "location": light.location,
            "look_at": light.look_at,
        });
        // The settings are flattened onto the create call, which is why one
        // call per light is enough.
        if let (Value::Object(target), Ok(Value::Object(settings))) =
            (&mut args, serde_json::to_value(&light.settings))
        {
            target.extend(settings);
        }
        if let Some(collection) = &collection_id
            && let Value::Object(target) = &mut args
        {
            target.insert("collection".into(), json!(collection));
        }

        let Some(created) = run
            .step(
                &format!("create the {} light", light.role.to_lowercase()),
                "light.create",
                args,
            )
            .await
        else {
            return run.finish(request.rollback_on_failure).await;
        };
        // A light the workflow cannot identify is a light it cannot clean up,
        // so a result with no id is a failure rather than something to skip
        // over quietly.
        let Some(id) = created
            .get("light")
            .and_then(|l| l.get("id"))
            .and_then(Value::as_str)
        else {
            run.fail(
                &format!("read the {} light id", light.role.to_lowercase()),
                blender_protocol::BlenderError::internal(
                    "light.create returned no id, so the light could not be tracked",
                ),
            );
            return run.finish(request.rollback_on_failure).await;
        };
        run.created(
            &format!("{}_light", light.role.to_lowercase()),
            json!({"id": id, "name": light.name}),
        );
        run.compensate(crate::rollback::Compensation::delete_object(id));
    }

    run.finish(request.rollback_on_failure).await
}

/// Reconstruct world bounds from an `object.get` result.
///
/// The bridge reports a location and dimensions rather than a box, because that
/// is what Blender exposes cheaply; turning it back into bounds here keeps the
/// rig maths in one place.
pub fn bounds_from_object(result: &Value) -> Option<Aabb> {
    let object = result.get("object")?;
    let location = vec3(object.get("location")?)?;
    let dimensions = vec3(object.get("dimensions")?)?;
    if dimensions.length() < 1e-9 {
        return None;
    }
    let half = dimensions * 0.5;
    Some(Aabb::new(location - half, location + half))
}

fn vec3(value: &Value) -> Option<blender_protocol::math::Vec3> {
    Some(blender_protocol::math::Vec3::new(
        value.get("x")?.as_f64()?,
        value.get("y")?.as_f64()?,
        value.get("z")?.as_f64()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn object(dimensions: (f64, f64, f64)) -> Value {
        json!({"object": {
            "id": "obj-1",
            "name": "Hero",
            "location": {"x": 0.0, "y": 0.0, "z": 0.0},
            "dimensions": {"x": dimensions.0, "y": dimensions.1, "z": dimensions.2},
        }})
    }

    fn request() -> ThreePointRequest {
        ThreePointRequest {
            target: ObjectRef::name("Hero"),
            key_energy: Some(800.0),
            fill_ratio: None,
            rim_ratio: None,
            distance_factor: None,
            key_temperature: None,
            collection: Some("Lights".into()),
            name_prefix: None,
            rollback_on_failure: true,
        }
    }

    #[tokio::test]
    async fn the_rig_creates_three_lights_aimed_at_the_subject() {
        let recorder = Recorder::new(json!({"light": {"id": "l1", "name": "Key"}}))
            .expect("object.get", Reply::Ok(object((2.0, 2.0, 2.0))));
        let report = three_point(&recorder, request()).await;
        assert!(report.success, "{report:?}");

        let creates = recorder
            .ops()
            .into_iter()
            .filter(|o| o == "light.create")
            .count();
        assert_eq!(creates, 3);
        for role in ["key_light", "fill_light", "rim_light"] {
            assert!(report.created.contains_key(role), "missing {role}");
        }
    }

    #[tokio::test]
    async fn every_light_is_told_what_to_look_at() {
        let recorder = Recorder::new(json!({"light": {"id": "l1"}}))
            .expect("object.get", Reply::Ok(object((2.0, 2.0, 2.0))));
        three_point(&recorder, request()).await;
        let create = recorder.args_for("light.create").unwrap();
        assert!(create.get("look_at").is_some(), "got {create}");
        assert!(
            create.get("energy").is_some(),
            "settings should be flattened in"
        );
    }

    #[tokio::test]
    async fn a_subject_with_no_size_is_refused_with_a_reason() {
        let recorder = Recorder::new(json!({"light": {"id": "l1"}}))
            .expect("object.get", Reply::Ok(object((0.0, 0.0, 0.0))));
        let report = three_point(&recorder, request()).await;
        assert!(!report.success);
        assert!(
            report.error.unwrap().message.contains("nothing to light"),
            "the reason should be actionable"
        );
        assert!(!recorder.called("light.create"));
    }

    #[tokio::test]
    async fn a_failure_part_way_removes_the_lights_already_made() {
        let recorder = Recorder::new(json!({"light": {"id": "l1"}}))
            .expect("object.get", Reply::Ok(object((2.0, 2.0, 2.0))))
            .expect("light.create", Reply::Ok(json!({"light": {"id": "key"}})))
            .expect(
                "light.create",
                Reply::Fail(blender_protocol::BlenderError::invalid_argument("no")),
            );
        let report = three_point(&recorder, request()).await;
        assert!(!report.success);
        let rollback = report.rollback.expect("rollback");
        assert!(rollback.complete, "{rollback:?}");
        assert!(recorder.called("object.delete"));
    }

    #[tokio::test]
    async fn a_warm_key_keeps_a_cooler_fill() {
        let recorder = Recorder::new(json!({"light": {"id": "l1"}}))
            .expect("object.get", Reply::Ok(object((2.0, 2.0, 2.0))));
        let mut request = request();
        // A key warmer than the default fill would otherwise invert the
        // convention.
        request.key_temperature = Some(8000.0);
        three_point(&recorder, request).await;

        let temperatures: Vec<f64> = recorder
            .calls()
            .into_iter()
            .filter(|(op, _)| op == "light.create")
            .filter_map(|(_, args)| args.get("temperature").and_then(Value::as_f64))
            .collect();
        assert_eq!(temperatures.len(), 3);
        assert!(
            temperatures[1] > temperatures[0],
            "the fill {} should stay cooler than the key {}",
            temperatures[1],
            temperatures[0]
        );
    }

    #[test]
    fn bounds_are_reconstructed_from_location_and_size() {
        let bounds = bounds_from_object(&json!({"object": {
            "location": {"x": 1.0, "y": 2.0, "z": 3.0},
            "dimensions": {"x": 2.0, "y": 4.0, "z": 6.0},
        }}))
        .unwrap();
        assert_eq!(
            bounds.center(),
            blender_protocol::math::Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            bounds.size(),
            blender_protocol::math::Vec3::new(2.0, 4.0, 6.0)
        );
    }
}

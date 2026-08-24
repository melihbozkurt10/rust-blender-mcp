//! Render setup workflows.

use blender_domain::camera::{DEFAULT_DIRECTION, FramingRequest};
use blender_protocol::{
    Validate,
    ids::ObjectRef,
    math::{Color4, Vec3},
    render::RenderSettings,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Executor,
    rollback::Compensation,
    run::{Run, WorkflowReport},
    workflows::lighting::{ThreePointRequest, three_point},
};

/// `workflow.render.studio`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StudioRequest {
    /// What the studio is set up around.
    pub subject: ObjectRef,
    /// Reuse this camera rather than creating one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    /// Skip the lighting rig, for a scene that already has one.
    #[serde(default)]
    pub skip_lighting: bool,
    /// Background colour. A mid grey by default, which is what a studio
    /// backdrop looks like and what keeps exposure honest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color4>,
    /// Render on a transparent film instead of a background colour.
    #[serde(default)]
    pub transparent: bool,
    /// Padding around the subject when framing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<f64>,
    /// Which direction to view from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Vec3>,
    #[serde(default, flatten)]
    pub render: RenderSettings,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

impl Validate for StudioRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        self.render.validate()?;
        if let Some(padding) = self.padding {
            blender_protocol::math::check_range(padding, 0.0, 10.0, "padding")?;
        }
        if self.transparent && self.background.is_some() {
            return Err(blender_protocol::BlenderError::invalid_argument(
                "`transparent` and `background` contradict each other: a transparent film has no \
                 background to colour.",
            ));
        }
        Ok(())
    }
}

/// Set up a studio shot: camera, lighting, world and render settings.
pub async fn studio_render(executor: &dyn Executor, request: StudioRequest) -> WorkflowReport {
    let mut run = Run::new("workflow.render.studio", executor);

    let Some(subject) = run
        .step(
            "measure the subject",
            "object.get",
            json!({"object": request.subject}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };

    let Some(bounds) = super::lighting::bounds_from_object(&subject) else {
        run.fail(
            "measure the subject",
            blender_protocol::BlenderError::invalid_argument(
                "The subject reported no size, so there is nothing to frame or light.",
            ),
        );
        return run.finish(request.rollback_on_failure).await;
    };

    // Work the shot out before touching the scene.
    let resolution_x = request.render.resolution_x.unwrap_or(1920) as f64;
    let resolution_y = request.render.resolution_y.unwrap_or(1080) as f64;
    let mut framing = FramingRequest::new(
        bounds,
        // A 50mm lens on a 36mm sensor, which is the neutral default a product
        // shot wants: wide enough to be readable, long enough not to distort.
        2.0 * ((36.0 / 2.0) / 50.0_f64).atan(),
        resolution_x / resolution_y.max(1.0),
    );
    framing.padding = request.padding.unwrap_or(0.15);
    framing.direction = request.direction.unwrap_or(DEFAULT_DIRECTION);

    let shot = match framing.solve() {
        Ok(shot) => shot,
        Err(error) => {
            run.fail("work out the shot", error);
            return run.finish(request.rollback_on_failure).await;
        }
    };
    run.note(
        "work out the shot",
        json!({
            "location": shot.location,
            "distance": shot.distance,
            "radius": shot.radius,
            "target": shot.target,
        }),
    );

    // Camera: reuse or create.
    let camera_id = match &request.camera {
        Some(existing) => {
            run.step(
                "place the camera",
                "object.transform",
                json!({"object": existing, "location": shot.location}),
            )
            .await;
            run.step(
                "aim the camera",
                "camera.look_at",
                json!({"camera": existing, "point": shot.target}),
            )
            .await;
            Some(existing.to_string())
        }
        None => {
            let created = run
                .step(
                    "create the camera",
                    "camera.create",
                    json!({
                        "name": "StudioCamera",
                        "location": shot.location,
                        "look_at": shot.target,
                        "set_active": true,
                        "lens": {"millimetres": 50.0},
                    }),
                )
                .await;
            let Some(created) = created else {
                return run.finish(request.rollback_on_failure).await;
            };
            let id = created
                .get("camera")
                .and_then(|c| c.get("id"))
                .and_then(Value::as_str);
            if let Some(id) = id {
                run.created(
                    "camera",
                    created.get("camera").cloned().unwrap_or(Value::Null),
                );
                run.compensate(Compensation::delete_object(id));
            }
            id.map(str::to_owned)
        }
    };

    if let Some(camera) = &camera_id {
        run.optional_step(
            "focus on the subject",
            "camera.depth_of_field.update",
            json!({"camera": camera, "enabled": true, "focus_distance": shot.distance, "f_stop": 5.6}),
        )
        .await;
    }

    // World.
    if request.transparent {
        run.step(
            "make the film transparent",
            "scene.world.update",
            json!({"transparent": true}),
        )
        .await;
    } else {
        let background = request.background.unwrap_or(Color4::rgb(0.05, 0.05, 0.05));
        run.step(
            "set the background",
            "scene.world.update",
            json!({"color": background, "strength": 1.0, "transparent": false}),
        )
        .await;
    }

    // Lighting.
    if !request.skip_lighting {
        let lighting = three_point(
            executor,
            ThreePointRequest {
                target: request.subject.clone(),
                key_energy: None,
                fill_ratio: None,
                rim_ratio: None,
                distance_factor: None,
                key_temperature: None,
                collection: None,
                name_prefix: Some("Studio".into()),
                rollback_on_failure: false,
            },
        )
        .await;

        run.note(
            "build the lighting rig",
            serde_json::to_value(&lighting).unwrap_or(Value::Null),
        );
        for (role, entity) in &lighting.created {
            run.created(role, entity.clone());
            if let Some(id) = entity.get("id").and_then(Value::as_str) {
                run.compensate(Compensation::delete_object(id));
            }
        }
        if !lighting.success {
            run.fail(
                "build the lighting rig",
                blender_protocol::BlenderError::internal(
                    lighting
                        .error
                        .as_ref()
                        .map(|e| e.message.clone())
                        .unwrap_or_else(|| "the lighting rig failed".into()),
                ),
            );
            return run.finish(request.rollback_on_failure).await;
        }
    }

    // Render settings last, so a rejected setting does not leave a half-built
    // studio behind.
    let mut settings = serde_json::to_value(&request.render).unwrap_or(json!({}));
    if let Value::Object(map) = &mut settings
        && map.is_empty()
    {
        map.insert("resolution_x".into(), json!(1920));
        map.insert("resolution_y".into(), json!(1080));
    }
    run.step(
        "apply the render settings",
        "render.settings.update",
        settings,
    )
    .await;

    run.finish(request.rollback_on_failure).await
}

/// `workflow.product_turntable`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TurntableRequest {
    /// The object to spin around.
    pub subject: ObjectRef,
    /// How many frames one full turn takes.
    #[serde(default = "default_frames")]
    pub frames: u32,
    /// Degrees of rotation over those frames.
    #[serde(default = "default_degrees")]
    pub degrees: f64,
    /// Build a studio setup first.
    #[serde(default = "super::material::default_true")]
    pub setup_studio: bool,
    /// Render the frames as well as setting the animation up.
    #[serde(default)]
    pub render: bool,
    #[serde(default, flatten)]
    pub render_settings: RenderSettings,
    #[serde(default = "super::material::default_true")]
    pub rollback_on_failure: bool,
}

fn default_frames() -> u32 {
    120
}
fn default_degrees() -> f64 {
    360.0
}

impl Validate for TurntableRequest {
    fn validate(&self) -> blender_protocol::Result<()> {
        self.render_settings.validate()?;
        if self.frames == 0 || self.frames > 3600 {
            return Err(blender_protocol::BlenderError::invalid_argument(format!(
                "`frames` must be between 1 and 3600, got {}.",
                self.frames
            )));
        }
        Ok(())
    }
}

/// Build a turntable: an orbiting camera, a studio setup, and optionally the
/// rendered frames.
///
/// The subject is never rotated. Spinning the object itself changes the scene
/// in a way the caller did not ask for and breaks anything parented to it, so
/// an empty is created, the camera is parented to it, and the empty spins.
pub async fn product_turntable(
    executor: &dyn Executor,
    request: TurntableRequest,
) -> WorkflowReport {
    let mut run = Run::new("workflow.product_turntable", executor);

    let Some(subject) = run
        .step(
            "measure the subject",
            "object.get",
            json!({"object": request.subject}),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(bounds) = super::lighting::bounds_from_object(&subject) else {
        run.fail(
            "measure the subject",
            blender_protocol::BlenderError::invalid_argument(
                "The subject reported no size, so there is nothing to turn around.",
            ),
        );
        return run.finish(request.rollback_on_failure).await;
    };
    let centre = bounds.center();

    // A studio setup gives the camera and the lights.
    let mut camera_id = None;
    if request.setup_studio {
        let studio = studio_render(
            executor,
            StudioRequest {
                subject: request.subject.clone(),
                camera: None,
                skip_lighting: false,
                background: None,
                transparent: false,
                padding: Some(0.2),
                direction: None,
                render: request.render_settings.clone(),
                rollback_on_failure: false,
            },
        )
        .await;
        run.note(
            "set up the studio",
            serde_json::to_value(&studio).unwrap_or(Value::Null),
        );
        for (role, entity) in &studio.created {
            run.created(role, entity.clone());
            if let Some(id) = entity.get("id").and_then(Value::as_str) {
                run.compensate(Compensation::delete_object(id));
            }
        }
        if !studio.success {
            run.fail(
                "set up the studio",
                blender_protocol::BlenderError::internal("the studio setup failed"),
            );
            return run.finish(request.rollback_on_failure).await;
        }
        camera_id = studio
            .created
            .get("camera")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    // The pivot the camera orbits on.
    let Some(pivot) = run
        .step(
            "create the turntable pivot",
            "object.create",
            json!({
                "type": "EMPTY",
                "name": "TurntablePivot",
                "location": centre,
                "options": {"empty_display_type": "PLAIN_AXES", "size": bounds.bounding_radius()},
            }),
        )
        .await
    else {
        return run.finish(request.rollback_on_failure).await;
    };
    let Some(pivot_id) = run.created_object("pivot", &pivot) else {
        run.fail(
            "read the pivot id",
            blender_protocol::BlenderError::internal("object.create returned no id"),
        );
        return run.finish(request.rollback_on_failure).await;
    };

    if let Some(camera) = &camera_id {
        run.step(
            "parent the camera to the pivot",
            "object.set_parent",
            json!({"object": camera, "parent": pivot_id, "keep_transform": true}),
        )
        .await;
    }

    run.step(
        "set the frame range",
        "animation.range.set",
        json!({"frame_start": 1, "frame_end": request.frames}),
    )
    .await;

    run.step(
        "animate the turn",
        "animation.create_rotation",
        json!({
            "object": pivot_id,
            "start_frame": 1,
            "end_frame": request.frames,
            "axis": "Z",
            "degrees": request.degrees,
            // Linear, or the turn eases in and out and looks wrong on a loop.
            "interpolation": "LINEAR",
        }),
    )
    .await;

    if request.render {
        run.step(
            "render the frames",
            "render.execute",
            json!({
                "scope": {"range": {"start": 1, "end": request.frames, "step": 1}},
                "name": "turntable",
            }),
        )
        .await;
    }

    run.finish(request.rollback_on_failure).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn subject() -> Value {
        json!({"object": {
            "id": "sub-1",
            "name": "Hero",
            "location": {"x": 0.0, "y": 0.0, "z": 0.0},
            "dimensions": {"x": 2.0, "y": 2.0, "z": 2.0},
        }})
    }

    fn studio_recorder() -> Recorder {
        // The default answer carries a light id, because that is what
        // `light.create` really returns and the workflow refuses to proceed
        // without one.
        Recorder::new(json!({"light": {"id": "light-1"}, "ok": true}))
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "camera.create",
                Reply::Ok(json!({"camera": {"id": "cam-1"}})),
            )
            .expect("object.get", Reply::Ok(subject()))
    }

    fn studio_request() -> StudioRequest {
        StudioRequest {
            subject: ObjectRef::name("Hero"),
            camera: None,
            skip_lighting: false,
            background: None,
            transparent: false,
            padding: None,
            direction: None,
            render: RenderSettings::default(),
            rollback_on_failure: true,
        }
    }

    #[tokio::test]
    async fn a_studio_creates_a_camera_lights_a_world_and_settings() {
        let recorder = studio_recorder();
        let report = studio_render(&recorder, studio_request()).await;
        assert!(report.success, "{report:?}");

        let ops = recorder.ops();
        assert!(ops.contains(&"camera.create".to_string()));
        assert!(ops.contains(&"scene.world.update".to_string()));
        assert!(ops.contains(&"render.settings.update".to_string()));
        assert_eq!(ops.iter().filter(|o| *o == "light.create").count(), 3);
        assert!(report.created.contains_key("camera"));
        assert!(report.created.contains_key("key_light"));
    }

    #[tokio::test]
    async fn the_shot_is_worked_out_before_the_camera_is_created() {
        let recorder = studio_recorder();
        let report = studio_render(&recorder, studio_request()).await;
        let shot = report
            .steps
            .iter()
            .find(|s| s.name == "work out the shot")
            .unwrap();
        assert!(shot.op.is_none());
        let distance = shot.result.as_ref().unwrap()["distance"].as_f64().unwrap();
        assert!(distance > 0.0);
    }

    #[tokio::test]
    async fn transparent_and_a_background_colour_are_refused_together() {
        let mut request = studio_request();
        request.transparent = true;
        request.background = Some(Color4::WHITE);
        assert!(request.validate().is_err());
    }

    #[tokio::test]
    async fn skipping_lighting_makes_no_lights() {
        let recorder = studio_recorder();
        let mut request = studio_request();
        request.skip_lighting = true;
        let report = studio_render(&recorder, request).await;
        assert!(report.success);
        assert!(!recorder.called("light.create"));
    }

    #[tokio::test]
    async fn a_studio_failure_removes_the_camera_and_lights() {
        let recorder = Recorder::new(json!({"light": {"id": "light-1"}, "ok": true}))
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "camera.create",
                Reply::Ok(json!({"camera": {"id": "cam-1"}})),
            )
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "render.settings.update",
                Reply::Fail(blender_protocol::BlenderError::invalid_argument("bad")),
            );
        let report = studio_render(&recorder, studio_request()).await;
        assert!(!report.success);
        let deletes = recorder
            .ops()
            .into_iter()
            .filter(|o| o == "object.delete")
            .count();
        assert!(deletes >= 4, "camera plus three lights, got {deletes}");
    }

    #[tokio::test]
    async fn a_turntable_spins_a_pivot_not_the_subject() {
        let recorder = Recorder::new(json!({"light": {"id": "light-1"}, "ok": true}))
            .expect("object.get", Reply::Ok(subject()))
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "camera.create",
                Reply::Ok(json!({"camera": {"id": "cam-1"}})),
            )
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "object.create",
                Reply::Ok(json!({"object": {"id": "pivot-1"}})),
            );

        let report = product_turntable(
            &recorder,
            TurntableRequest {
                subject: ObjectRef::name("Hero"),
                frames: 60,
                degrees: 360.0,
                setup_studio: true,
                render: false,
                render_settings: RenderSettings::default(),
                rollback_on_failure: true,
            },
        )
        .await;

        assert!(report.success, "{report:?}");
        let rotation = recorder.args_for("animation.create_rotation").unwrap();
        assert_eq!(
            rotation["object"], "pivot-1",
            "the pivot spins, not the subject"
        );
        assert_eq!(
            rotation["interpolation"], "LINEAR",
            "a looping turn must not ease"
        );
        assert_eq!(rotation["end_frame"], 60);

        let parent = recorder.args_for("object.set_parent").unwrap();
        assert_eq!(parent["parent"], "pivot-1");
        assert_eq!(parent["object"], "cam-1");
    }

    #[tokio::test]
    async fn a_turntable_only_renders_when_asked() {
        let recorder = Recorder::new(json!({"light": {"id": "light-1"}, "ok": true}))
            .expect("object.get", Reply::Ok(subject()))
            .expect(
                "object.create",
                Reply::Ok(json!({"object": {"id": "pivot-1"}})),
            );
        let report = product_turntable(
            &recorder,
            TurntableRequest {
                subject: ObjectRef::name("Hero"),
                frames: 24,
                degrees: 360.0,
                setup_studio: false,
                render: false,
                render_settings: RenderSettings::default(),
                rollback_on_failure: true,
            },
        )
        .await;
        assert!(report.success, "{report:?}");
        assert!(!recorder.called("render.execute"));
    }

    #[tokio::test]
    async fn an_absurd_frame_count_is_refused_before_anything_runs() {
        let request = TurntableRequest {
            subject: ObjectRef::name("Hero"),
            frames: 100_000,
            degrees: 360.0,
            setup_studio: false,
            render: false,
            render_settings: RenderSettings::default(),
            rollback_on_failure: true,
        };
        assert!(request.validate().is_err());
    }
}

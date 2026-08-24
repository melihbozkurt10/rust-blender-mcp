//! Camera payloads, including automatic framing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{CameraId, CollectionRef, ObjectRef},
    math::{Finite, Vec2, Vec3, check_positive, check_range},
};

/// Camera projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectionType {
    Perspective,
    Orthographic,
    Panoramic,
}

/// How the lens is specified.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lens {
    /// Focal length in millimetres.
    Millimetres(f64),
    /// Horizontal field of view in degrees.
    FovDegrees(f64),
}

impl Lens {
    /// Focal length for a given sensor width.
    pub fn focal_length(self, sensor_width_mm: f64) -> f64 {
        match self {
            Lens::Millimetres(mm) => mm,
            Lens::FovDegrees(degrees) => {
                let half = (degrees.to_radians() * 0.5).tan();
                if half.abs() < 1e-9 {
                    f64::MAX
                } else {
                    (sensor_width_mm * 0.5) / half
                }
            }
        }
    }

    /// Horizontal field of view in radians for a given sensor width.
    pub fn fov_radians(self, sensor_width_mm: f64) -> f64 {
        match self {
            Lens::Millimetres(mm) => 2.0 * ((sensor_width_mm * 0.5) / mm).atan(),
            Lens::FovDegrees(degrees) => degrees.to_radians(),
        }
    }
}

impl Validate for Lens {
    fn validate(&self) -> Result<()> {
        match self {
            Lens::Millimetres(mm) => check_range(*mm, 1.0, 5000.0, "lens.millimetres"),
            Lens::FovDegrees(degrees) => check_range(*degrees, 0.1, 179.0, "lens.fov_degrees"),
        }
    }
}

/// Depth-of-field settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DepthOfField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Object to keep in focus. Overrides `focus_distance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_object: Option<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_distance: Option<f64>,
    /// Aperture. Lower is shallower depth of field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_stop: Option<f64>,
    /// Aperture blade count. 0 gives a perfectly circular bokeh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blades: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
}

impl Validate for DepthOfField {
    fn validate(&self) -> Result<()> {
        if let Some(distance) = self.focus_distance {
            check_positive(distance, "focus_distance")?;
        }
        if let Some(f_stop) = self.f_stop {
            check_range(f_stop, 0.1, 128.0, "f_stop")?;
        }
        if let Some(blades) = self.blades
            && blades != 0
            && !(3..=16).contains(&blades)
        {
            return Err(BlenderError::invalid_argument(format!(
                "`blades` must be 0 (circular) or between 3 and 16, got {blades}."
            )));
        }
        if let Some(ratio) = self.ratio {
            check_positive(ratio, "ratio")?;
        }
        if self.focus_object.is_some() && self.focus_distance.is_some() {
            return Err(BlenderError::invalid_argument(
                "`focus_object` and `focus_distance` conflict; a focus object drives the distance.",
            ));
        }
        Ok(())
    }
}

/// Camera data-block settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CameraSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<Lens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<ProjectionType>,
    /// Orthographic view height in Blender units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ortho_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_height: Option<f64>,
    /// `AUTO`, `HORIZONTAL` or `VERTICAL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_fit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_end: Option<f64>,
    /// Lens shift, in units of sensor width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<Vec2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<DepthOfField>,
}

impl Validate for CameraSettings {
    fn validate(&self) -> Result<()> {
        if let Some(lens) = self.lens {
            lens.validate()?;
        }
        for (value, field) in [
            (self.ortho_scale, "ortho_scale"),
            (self.sensor_width, "sensor_width"),
            (self.sensor_height, "sensor_height"),
            (self.clip_start, "clip_start"),
            (self.clip_end, "clip_end"),
        ] {
            if let Some(v) = value {
                check_positive(v, field)?;
            }
        }
        if let (Some(start), Some(end)) = (self.clip_start, self.clip_end)
            && start >= end
        {
            return Err(BlenderError::invalid_argument(format!(
                "`clip_start` ({start}) must be less than `clip_end` ({end})."
            )));
        }
        if let Some(shift) = self.shift {
            shift.check_finite("shift")?;
        }
        if let Some(fit) = &self.sensor_fit {
            const FITS: [&str; 3] = ["AUTO", "HORIZONTAL", "VERTICAL"];
            if !FITS.contains(&fit.as_str()) {
                return Err(BlenderError::invalid_enum("sensor_fit", fit.clone(), FITS));
            }
        }
        if let Some(dof) = &self.depth_of_field {
            dof.validate()?;
        }
        Ok(())
    }
}

/// `camera.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateCamera {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Vec3>,
    /// Aim at this point instead of specifying a rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_at: Option<Vec3>,
    /// Frame these objects instead of specifying a location.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_objects: Vec<ObjectRef>,
    /// Make this the scene's active camera.
    #[serde(default)]
    pub set_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    #[serde(default, flatten)]
    pub settings: CameraSettings,
}

impl Validate for CreateCamera {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.location.check_finite("location")?;
        self.rotation.check_finite("rotation")?;
        self.look_at.check_finite("look_at")?;
        if self.rotation.is_some() && self.look_at.is_some() {
            return Err(BlenderError::invalid_argument(
                "Set `rotation` or `look_at`, not both.",
            ));
        }
        if !self.frame_objects.is_empty() && self.location.is_some() {
            return Err(BlenderError::invalid_argument(
                "`frame_objects` computes the location; remove `location` or remove `frame_objects`.",
            ));
        }
        self.settings.validate()
    }
}

/// `camera.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateCamera {
    pub camera: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, flatten)]
    pub settings: CameraSettings,
}

impl Validate for UpdateCamera {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.settings.validate()
    }
}

/// `camera.track_object` -- add a constraint so the camera keeps aiming at a
/// target as either moves.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TrackObject {
    pub camera: ObjectRef,
    pub target: ObjectRef,
    /// `TRACK_TO` (with an up axis) or `DAMPED_TRACK` (shortest rotation).
    #[serde(default = "default_track_type")]
    pub constraint: TrackConstraint,
    /// Also set the camera's depth of field focus to the target.
    #[serde(default)]
    pub focus_on_target: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrackConstraint {
    TrackTo,
    DampedTrack,
    LockedTrack,
}

fn default_track_type() -> TrackConstraint {
    TrackConstraint::TrackTo
}

impl Validate for TrackObject {
    fn validate(&self) -> Result<()> {
        if self.camera == self.target {
            return Err(BlenderError::invalid_argument(
                "A camera cannot track itself.",
            ));
        }
        Ok(())
    }
}

/// `camera.auto_frame` -- place the camera so the given objects fill the frame.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AutoFrame {
    /// Camera to move. Omit to use the scene's active camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    /// Objects to fit in frame. Empty means every visible object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    /// Extra margin around the subject, as a fraction of its size.
    #[serde(default = "default_padding")]
    pub padding: f64,
    /// Direction to view from, in world space. Normalised server-side.
    /// Defaults to a three-quarter view from the front-left and slightly above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Vec3>,
    /// Keep the camera where it is and only adjust the lens.
    #[serde(default)]
    pub keep_position: bool,
    /// Also aim the camera at the subject's centre.
    #[serde(default = "crate::object::default_true")]
    pub aim: bool,
    /// Set the depth-of-field focus distance to the subject's centre.
    #[serde(default)]
    pub focus: bool,
}

fn default_padding() -> f64 {
    0.1
}

impl Validate for AutoFrame {
    fn validate(&self) -> Result<()> {
        check_range(self.padding, 0.0, 10.0, "padding")?;
        if let Some(direction) = self.direction {
            direction.check_finite("direction")?;
            if direction.normalized().is_none() {
                return Err(BlenderError::invalid_argument(
                    "`direction` has zero length, so it does not describe a direction.",
                ));
            }
        }
        if self.keep_position && self.direction.is_some() {
            return Err(BlenderError::invalid_argument(
                "`direction` moves the camera, which `keep_position` forbids.",
            ));
        }
        Ok(())
    }
}

/// A camera as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CameraSummary {
    /// Id of the camera *object*.
    pub id: crate::ids::ObjectId,
    /// Id of the camera data-block.
    pub data_id: CameraId,
    pub name: String,
    pub location: Vec3,
    pub rotation_euler: Vec3,
    pub lens_mm: f64,
    pub sensor_width: f64,
    pub sensor_height: f64,
    pub projection: ProjectionType,
    pub clip_start: f64,
    pub clip_end: f64,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<DepthOfField>,
    /// Constraints currently on the camera object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

/// `camera.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListCameras {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListCameras {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fov_and_focal_length_agree() {
        let sensor = 36.0;
        let lens = Lens::Millimetres(50.0);
        let fov = lens.fov_radians(sensor);
        let back = Lens::FovDegrees(fov.to_degrees()).focal_length(sensor);
        assert!((back - 50.0).abs() < 1e-9, "round trip gave {back}");
    }

    #[test]
    fn absurd_lenses_are_rejected() {
        assert!(Lens::Millimetres(0.0).validate().is_err());
        assert!(Lens::FovDegrees(180.0).validate().is_err());
        assert!(Lens::FovDegrees(60.0).validate().is_ok());
    }

    #[test]
    fn clip_planes_must_be_ordered() {
        let settings = CameraSettings {
            clip_start: Some(10.0),
            clip_end: Some(1.0),
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn focus_object_and_distance_conflict() {
        let dof = DepthOfField {
            focus_object: Some(ObjectRef::name("Cube")),
            focus_distance: Some(4.0),
            ..Default::default()
        };
        assert!(dof.validate().is_err());
    }

    #[test]
    fn auto_frame_rejects_a_zero_direction() {
        let params = AutoFrame {
            camera: None,
            objects: vec![],
            padding: 0.1,
            direction: Some(Vec3::ZERO),
            keep_position: false,
            aim: true,
            focus: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn blade_counts_follow_blender() {
        let ok = DepthOfField {
            blades: Some(0),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let ok = DepthOfField {
            blades: Some(6),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let bad = DepthOfField {
            blades: Some(2),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }
}

/// `camera.get` / `camera.delete` / `camera.set_active` / `camera.clear_tracking`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CameraRefParams {
    /// Camera object. Omit to use the scene's active camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
}

/// `camera.look_at`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CameraLookAt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    /// Explicit world-space aim point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<Vec3>,
    /// Aim at an object's bounding-box centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
}

/// `camera.depth_of_field.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateDepthOfField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    #[serde(flatten)]
    pub depth_of_field: DepthOfField,
}

impl Validate for CameraRefParams {}

impl Validate for CameraLookAt {
    fn validate(&self) -> Result<()> {
        match (self.point, &self.target) {
            (None, None) => Err(BlenderError::invalid_argument(
                "Provide `point` or `target`.",
            )),
            (Some(_), Some(_)) => Err(BlenderError::invalid_argument(
                "Provide `point` or `target`, not both.",
            )),
            _ => self.point.check_finite("point"),
        }
    }
}

impl Validate for UpdateDepthOfField {
    fn validate(&self) -> Result<()> {
        self.depth_of_field.validate()
    }
}

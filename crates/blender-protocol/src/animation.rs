//! Animation payloads: keyframes, F-curves, actions, interpolation and NLA.
//!
//! The design goal is that ordinary animation never requires touching F-curve
//! internals. A caller says "rotate this object 360 degrees over 120 frames"
//! and the server expands that into deterministic keyframe inserts; F-curve
//! access exists for the cases that genuinely need it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_frame_range, check_name,
    ids::{ActionId, ActionRef, ObjectRef},
    math::{Axis, Finite, Vec3, check_positive},
};

/// Which property a keyframe targets.
///
/// The common cases are named so callers never have to build an RNA data path
/// by hand; `Custom` remains for the rest, and is validated as a data path
/// rather than accepted blindly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyTarget {
    Location,
    /// XYZ Euler rotation.
    RotationEuler,
    RotationQuaternion,
    Scale,
    /// Object visibility in the viewport.
    HideViewport,
    /// Object visibility in renders.
    HideRender,
    /// A shape key's value, by name.
    ShapeKey {
        name: String,
    },
    /// A custom property on the object, by name.
    CustomProperty {
        name: String,
    },
    /// A bone's transform channel, for pose animation.
    Bone {
        name: String,
        channel: BoneChannel,
    },
    /// A material's Principled input, by socket identifier.
    MaterialInput {
        material: String,
        socket: String,
    },
    /// An explicit RNA data path, for anything not covered above.
    DataPath {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<i32>,
    },
}

/// Transform channels a bone keyframe can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoneChannel {
    Location,
    RotationEuler,
    RotationQuaternion,
    Scale,
}

impl KeyTarget {
    /// A stable label used in errors and diagnostics.
    pub fn label(&self) -> String {
        match self {
            KeyTarget::Location => "location".into(),
            KeyTarget::RotationEuler => "rotation_euler".into(),
            KeyTarget::RotationQuaternion => "rotation_quaternion".into(),
            KeyTarget::Scale => "scale".into(),
            KeyTarget::HideViewport => "hide_viewport".into(),
            KeyTarget::HideRender => "hide_render".into(),
            KeyTarget::ShapeKey { name } => format!("shape_key[{name}]"),
            KeyTarget::CustomProperty { name } => format!("custom[{name}]"),
            KeyTarget::Bone { name, channel } => format!("bone[{name}].{channel:?}"),
            KeyTarget::MaterialInput { material, socket } => {
                format!("material[{material}].{socket}")
            }
            KeyTarget::DataPath { path, .. } => path.clone(),
        }
    }
}

impl Validate for KeyTarget {
    fn validate(&self) -> Result<()> {
        match self {
            KeyTarget::DataPath { path, .. } => {
                if path.is_empty() {
                    return Err(BlenderError::invalid_argument("`path` must not be empty."));
                }
                // RNA paths are identifiers, dots, and bracketed subscripts.
                // Anything else -- parentheses, whitespace, semicolons -- is
                // not a data path and has no business being resolved.
                let allowed = |c: char| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '_' | '.' | '[' | ']' | '"' | '\'' | '-')
                };
                if !path.chars().all(allowed) {
                    return Err(BlenderError::new(
                        crate::ErrorCode::InvalidProperty,
                        format!("`{path}` is not a valid RNA data path."),
                    )
                    .with_detail("path", path.clone()));
                }
                Ok(())
            }
            KeyTarget::ShapeKey { name }
            | KeyTarget::CustomProperty { name }
            | KeyTarget::Bone { name, .. } => check_name(name, "name"),
            KeyTarget::MaterialInput { material, socket } => {
                check_name(material, "material")?;
                check_name(socket, "socket")
            }
            _ => Ok(()),
        }
    }
}

/// Keyframe interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Interpolation {
    Constant,
    Linear,
    #[default]
    Bezier,
    Sine,
    Quad,
    Cubic,
    Quart,
    Quint,
    Expo,
    Circ,
    Back,
    Bounce,
    Elastic,
}

impl Interpolation {
    /// Whether Blender treats this as an easing type, which additionally
    /// accepts an [`Easing`] direction.
    pub const fn is_easing(self) -> bool {
        !matches!(
            self,
            Interpolation::Constant | Interpolation::Linear | Interpolation::Bezier
        )
    }
}

/// Easing direction for the non-Bezier interpolation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Easing {
    #[default]
    Auto,
    EaseIn,
    EaseOut,
    EaseInOut,
}

/// One keyframe value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyValue {
    /// A single scalar, for one-dimensional targets.
    Scalar(f64),
    /// A three-component value, for location/rotation/scale.
    Vector(Vec3),
    /// A four-component value, for quaternions.
    Quaternion(crate::math::Quat),
    Bool(bool),
}

impl Finite for KeyValue {
    fn check_finite(&self, field: &str) -> Result<()> {
        match self {
            KeyValue::Scalar(v) => crate::math::check_scalar_finite(*v, field),
            KeyValue::Vector(v) => v.check_finite(field),
            KeyValue::Quaternion(q) => q.check_finite(field),
            KeyValue::Bool(_) => Ok(()),
        }
    }
}

/// One keyframe to insert.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Keyframe {
    pub frame: f64,
    /// Value to key. Omit to key the property's current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<KeyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolation: Option<Interpolation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing: Option<Easing>,
}

impl Validate for Keyframe {
    fn validate(&self) -> Result<()> {
        crate::math::check_scalar_finite(self.frame, "frame")?;
        if let Some(value) = &self.value {
            value.check_finite("value")?;
        }
        if let (Some(interpolation), Some(_)) = (self.interpolation, self.easing)
            && !interpolation.is_easing()
        {
            return Err(BlenderError::invalid_argument(format!(
                "`easing` has no effect with {interpolation:?} interpolation."
            ))
            .with_detail("interpolation", format!("{interpolation:?}")));
        }
        Ok(())
    }
}

/// `animation.keyframe.insert`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InsertKeyframes {
    pub object: ObjectRef,
    pub target: KeyTarget,
    pub keyframes: Vec<Keyframe>,
    /// Replace any keyframes already on those frames.
    #[serde(default = "crate::object::default_true")]
    pub replace: bool,
    /// Create the action if the object has none.
    #[serde(default = "crate::object::default_true")]
    pub create_action: bool,
}

impl Validate for InsertKeyframes {
    fn validate(&self) -> Result<()> {
        self.target.validate()?;
        if self.keyframes.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`keyframes` must not be empty.",
            ));
        }
        if self.keyframes.len() > 10_000 {
            return Err(BlenderError::invalid_argument(format!(
                "{} keyframes in one request is beyond what the bridge should apply in a single main-thread pass; split the request.",
                self.keyframes.len()
            )));
        }
        for keyframe in &self.keyframes {
            keyframe.validate()?;
        }
        Ok(())
    }
}

/// `animation.keyframe.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteKeyframes {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<KeyTarget>,
    /// Delete keyframes on these exact frames.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<f64>,
    /// Delete every keyframe in this inclusive range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(f64, f64)>,
}

impl Validate for DeleteKeyframes {
    fn validate(&self) -> Result<()> {
        if let Some(target) = &self.target {
            target.validate()?;
        }
        if self.frames.is_empty() && self.frame_range.is_none() {
            return Err(BlenderError::invalid_argument(
                "Provide `frames` or `frame_range`; deleting every keyframe needs an explicit range.",
            ));
        }
        if let Some((start, end)) = self.frame_range
            && end < start
        {
            return Err(BlenderError::invalid_argument(format!(
                "`frame_range` end ({end}) precedes start ({start})."
            )));
        }
        Ok(())
    }
}

/// `animation.interpolation.set`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetInterpolation {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<KeyTarget>,
    pub interpolation: Interpolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing: Option<Easing>,
    /// Restrict to keyframes inside this inclusive frame range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(f64, f64)>,
}

/// `animation.action.create` / `assign`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionAssignment {
    pub object: ObjectRef,
    /// Action to assign. Omit with `create: true` to make a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Create the action if it does not exist.
    #[serde(default)]
    pub create: bool,
    /// Give the action a fake user so it survives a file save with no assignment.
    #[serde(default)]
    pub fake_user: bool,
}

impl Validate for ActionAssignment {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if self.action.is_none() && !self.create {
            return Err(BlenderError::invalid_argument(
                "Provide `action`, or set `create: true` to make a new one.",
            ));
        }
        Ok(())
    }
}

/// An action as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionSummary {
    pub id: ActionId,
    pub name: String,
    pub frame_range: (f64, f64),
    #[serde(default)]
    pub fcurve_count: u32,
    #[serde(default)]
    pub keyframe_count: u32,
    #[serde(default)]
    pub users: u32,
    #[serde(default)]
    pub fake_user: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
}

/// An F-curve as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FCurveSummary {
    pub data_path: String,
    pub array_index: i32,
    #[serde(default)]
    pub keyframe_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_range: Option<(f64, f64)>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub locked: bool,
    /// Cycle/noise modifiers stacked on the curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<String>,
}

/// `animation.fcurve.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateFCurve {
    pub object: ObjectRef,
    pub data_path: String,
    #[serde(default)]
    pub array_index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// Extrapolation outside the keyed range: `CONSTANT` or `LINEAR`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extrapolation: Option<String>,
    /// Add a cycles modifier so the curve repeats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cyclic: Option<bool>,
}

/// A generated transform animation, used by the high-level helpers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GeneratedMotion {
    pub object: ObjectRef,
    pub start_frame: i32,
    pub end_frame: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolation: Option<Interpolation>,
    /// Keyframe the starting state as well as the end state. Without this a
    /// pre-existing key elsewhere in the action decides where the motion
    /// starts, which is rarely what was meant.
    #[serde(default = "crate::object::default_true")]
    pub key_start: bool,
}

impl Validate for GeneratedMotion {
    fn validate(&self) -> Result<()> {
        check_frame_range(self.start_frame, self.end_frame)?;
        if self.start_frame == self.end_frame {
            return Err(BlenderError::invalid_argument(
                "Start and end frames are identical; the motion would have zero duration.",
            ));
        }
        Ok(())
    }
}

/// `animation.create_rotation`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RotationMotion {
    #[serde(flatten)]
    pub motion: GeneratedMotion,
    #[serde(default = "default_axis")]
    pub axis: Axis,
    /// Total rotation in degrees. 360 gives one full turn.
    #[serde(default = "default_full_turn")]
    pub degrees: f64,
    /// Add a cycles modifier so the rotation repeats forever.
    #[serde(default)]
    pub loop_forever: bool,
}

fn default_axis() -> Axis {
    Axis::Z
}

fn default_full_turn() -> f64 {
    360.0
}

impl Validate for RotationMotion {
    fn validate(&self) -> Result<()> {
        self.motion.validate()?;
        crate::math::check_scalar_finite(self.degrees, "degrees")?;
        if self.degrees == 0.0 {
            return Err(BlenderError::invalid_argument(
                "`degrees` of 0 produces no motion.",
            ));
        }
        // A linear interpolation is nearly always wanted for a turntable; a
        // Bezier one eases in and out and looks wrong when looped. Warn by
        // rejecting only the clearly broken case, not the stylistic one.
        Ok(())
    }
}

/// `animation.create_move`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveMotion {
    #[serde(flatten)]
    pub motion: GeneratedMotion,
    /// Destination in world space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec3>,
    /// Displacement from the current position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Vec3>,
}

impl Validate for MoveMotion {
    fn validate(&self) -> Result<()> {
        self.motion.validate()?;
        match (self.to, self.by) {
            (None, None) => Err(BlenderError::invalid_argument("Provide `to` or `by`.")),
            (Some(_), Some(_)) => Err(BlenderError::invalid_argument(
                "Provide `to` or `by`, not both.",
            )),
            (Some(v), None) | (None, Some(v)) => v.check_finite("target"),
        }
    }
}

/// `animation.create_scale`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScaleMotion {
    #[serde(flatten)]
    pub motion: GeneratedMotion,
    pub to: Vec3,
}

impl Validate for ScaleMotion {
    fn validate(&self) -> Result<()> {
        self.motion.validate()?;
        self.to.check_finite("to")?;
        for (v, axis) in [(self.to.x, "x"), (self.to.y, "y"), (self.to.z, "z")] {
            check_positive(v, &format!("to.{axis}"))?;
        }
        Ok(())
    }
}

/// `animation.nla.strip.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateNlaStrip {
    pub object: ObjectRef,
    /// Track to place the strip on. Created if it does not exist.
    pub track: String,
    pub action: ActionRef,
    pub start_frame: f64,
    /// Strip end. Defaults to the action's own length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_frame: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `REPLACE`, `COMBINE`, `ADD`, `SUBTRACT` or `MULTIPLY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub influence: Option<f64>,
    /// Number of times to repeat the action within the strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<f64>,
}

impl Validate for CreateNlaStrip {
    fn validate(&self) -> Result<()> {
        check_name(&self.track, "track")?;
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        crate::math::check_scalar_finite(self.start_frame, "start_frame")?;
        if let Some(end) = self.end_frame {
            crate::math::check_scalar_finite(end, "end_frame")?;
            if end <= self.start_frame {
                return Err(BlenderError::invalid_argument(
                    "`end_frame` must be after `start_frame`.",
                ));
            }
        }
        if let Some(influence) = self.influence {
            crate::math::check_range(influence, 0.0, 1.0, "influence")?;
        }
        if let Some(repeat) = self.repeat {
            check_positive(repeat, "repeat")?;
        }
        if let Some(blend) = &self.blend_type {
            const BLENDS: [&str; 5] = ["REPLACE", "COMBINE", "ADD", "SUBTRACT", "MULTIPLY"];
            if !BLENDS.contains(&blend.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "blend_type",
                    blend.clone(),
                    BLENDS,
                ));
            }
        }
        Ok(())
    }
}

/// `animation.keyframe.list` filters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListKeyframes {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<KeyTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(f64, f64)>,
    #[serde(default, flatten)]
    pub page: Page,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion() -> GeneratedMotion {
        GeneratedMotion {
            object: ObjectRef::name("Cube"),
            start_frame: 1,
            end_frame: 120,
            interpolation: Some(Interpolation::Linear),
            key_start: true,
        }
    }

    #[test]
    fn data_paths_reject_anything_that_is_not_a_path() {
        let bad = KeyTarget::DataPath {
            path: "location; __import__('os')".into(),
            index: None,
        };
        assert!(bad.validate().is_err());
        let good = KeyTarget::DataPath {
            path: "pose.bones[\"Head\"].location".into(),
            index: Some(0),
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn easing_on_bezier_is_rejected() {
        let key = Keyframe {
            frame: 1.0,
            value: None,
            interpolation: Some(Interpolation::Bezier),
            easing: Some(Easing::EaseIn),
        };
        assert!(key.validate().is_err());

        let key = Keyframe {
            interpolation: Some(Interpolation::Quad),
            ..key
        };
        assert!(key.validate().is_ok());
    }

    #[test]
    fn zero_length_motions_are_rejected() {
        let m = GeneratedMotion {
            end_frame: 1,
            ..motion()
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn rotation_needs_a_non_zero_angle() {
        let params = RotationMotion {
            motion: motion(),
            axis: Axis::Z,
            degrees: 0.0,
            loop_forever: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn move_needs_exactly_one_destination() {
        let base = MoveMotion {
            motion: motion(),
            to: None,
            by: None,
        };
        assert!(base.validate().is_err());
        assert!(
            MoveMotion {
                to: Some(Vec3::Z),
                ..base.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(
            MoveMotion {
                to: Some(Vec3::Z),
                by: Some(Vec3::Z),
                ..base
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn scale_to_zero_is_rejected() {
        let params = ScaleMotion {
            motion: motion(),
            to: Vec3::new(1.0, 0.0, 1.0),
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn nla_strips_must_have_positive_length() {
        let params = CreateNlaStrip {
            object: ObjectRef::name("Cube"),
            track: "Base".into(),
            action: ActionRef::name("Walk"),
            start_frame: 10.0,
            end_frame: Some(10.0),
            name: None,
            blend_type: None,
            influence: None,
            repeat: None,
        };
        assert!(params.validate().is_err());
    }
}

/// `animation.frame.set`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetFrame {
    pub frame: i32,
}

/// `animation.range.set`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SetFrameRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_start: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_end: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_step: Option<i32>,
}

/// `animation.action.list`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListActions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default, flatten)]
    pub page: Page,
}

/// `animation.action.get` / `animation.action.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionRefParams {
    pub action: ActionRef,
}

/// `animation.action.create`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CreateAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Assign the new action to this object straight away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ObjectRef>,
    /// Give it a fake user so it survives a save with no assignment.
    #[serde(default)]
    pub fake_user: bool,
}

/// `animation.fcurve.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListFCurves {
    pub object: ObjectRef,
    #[serde(default, flatten)]
    pub page: Page,
}

/// `animation.fcurve.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetFCurve {
    pub object: ObjectRef,
    /// RNA data path, e.g. `location`.
    pub data_path: String,
    /// Which component of a vector channel: 0 for X, 1 for Y, 2 for Z.
    #[serde(default)]
    pub array_index: i32,
}

/// `animation.loop`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoopAnimation {
    pub object: ObjectRef,
    /// False removes the cycles modifiers again.
    #[serde(default = "crate::object::default_true")]
    pub enabled: bool,
}

/// `animation.nla.track.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ObjectRefParams {
    pub object: ObjectRef,
}

/// `animation.nla.track.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateNlaTrack {
    pub object: ObjectRef,
    pub name: String,
}

/// `animation.nla.track.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NlaTrackRefParams {
    pub object: ObjectRef,
    pub track: String,
}

/// `animation.nla.strip.update` / `animation.nla.strip.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NlaStripRefParams {
    pub object: ObjectRef,
    pub track: String,
    pub strip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_frame: Option<f64>,
    /// `REPLACE`, `COMBINE`, `ADD`, `SUBTRACT` or `MULTIPLY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub influence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<f64>,
}

impl Validate for SetFrame {}
impl Validate for ActionRefParams {}
impl Validate for ObjectRefParams {}
impl Validate for GetFCurve {}
impl Validate for LoopAnimation {}
impl Validate for SetInterpolation {}
impl Validate for UpdateFCurve {}

impl Validate for SetFrameRange {
    fn validate(&self) -> Result<()> {
        if let (Some(start), Some(end)) = (self.frame_start, self.frame_end) {
            check_frame_range(start, end)?;
        }
        if let Some(step) = self.frame_step
            && step < 1
        {
            return Err(BlenderError::invalid_argument(
                "`frame_step` must be at least 1.",
            ));
        }
        if self.frame_start.is_none() && self.frame_end.is_none() && self.frame_step.is_none() {
            return Err(BlenderError::invalid_argument("Nothing to set."));
        }
        Ok(())
    }
}

impl Validate for ListActions {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

impl Validate for ListFCurves {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

impl Validate for ListKeyframes {
    fn validate(&self) -> Result<()> {
        self.page.validate()?;
        if let Some(target) = &self.target {
            target.validate()?;
        }
        Ok(())
    }
}

impl Validate for CreateAction {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        Ok(())
    }
}

impl Validate for CreateNlaTrack {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")
    }
}

impl Validate for NlaTrackRefParams {
    fn validate(&self) -> Result<()> {
        check_name(&self.track, "track")
    }
}

impl Validate for NlaStripRefParams {
    fn validate(&self) -> Result<()> {
        check_name(&self.track, "track")?;
        check_name(&self.strip, "strip")?;
        if let Some(influence) = self.influence {
            crate::math::check_range(influence, 0.0, 1.0, "influence")?;
        }
        if let Some(repeat) = self.repeat {
            check_positive(repeat, "repeat")?;
        }
        if let Some(blend) = &self.blend_type {
            const BLENDS: [&str; 5] = ["REPLACE", "COMBINE", "ADD", "SUBTRACT", "MULTIPLY"];
            if !BLENDS.contains(&blend.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "blend_type",
                    blend.clone(),
                    BLENDS,
                ));
            }
        }
        Ok(())
    }
}

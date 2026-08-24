//! Object-level payloads: creation, transforms, hierarchy and visibility.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{CollectionRef, MaterialRef, ObjectId, ObjectRef},
    math::{Finite, Vec3, check_positive},
};

/// Object types the bridge can create directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrimitiveType {
    Empty,
    Cube,
    Plane,
    UvSphere,
    IcoSphere,
    Cylinder,
    Cone,
    Torus,
    Monkey,
    Curve,
    Text,
    Camera,
    Light,
}

impl PrimitiveType {
    /// The Blender object type this primitive produces.
    pub const fn object_type(self) -> &'static str {
        match self {
            PrimitiveType::Empty => "EMPTY",
            PrimitiveType::Curve => "CURVE",
            PrimitiveType::Text => "FONT",
            PrimitiveType::Camera => "CAMERA",
            PrimitiveType::Light => "LIGHT",
            _ => "MESH",
        }
    }

    pub const fn is_mesh(self) -> bool {
        matches!(
            self,
            PrimitiveType::Cube
                | PrimitiveType::Plane
                | PrimitiveType::UvSphere
                | PrimitiveType::IcoSphere
                | PrimitiveType::Cylinder
                | PrimitiveType::Cone
                | PrimitiveType::Torus
                | PrimitiveType::Monkey
        )
    }
}

/// Blender object types, as reported by `object.type`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    Mesh,
    Curve,
    Surface,
    Meta,
    Font,
    Armature,
    Lattice,
    Empty,
    Gpencil,
    Camera,
    Light,
    Speaker,
    Volume,
    #[serde(other)]
    Other,
}

/// Rotation input. Callers pick whichever is natural; the bridge sets the
/// matching `rotation_mode` so the two never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    /// XYZ Euler angles in radians.
    Euler(Vec3),
    /// XYZ Euler angles in degrees, converted server-side.
    Degrees(Vec3),
    /// Quaternion in Blender's `(w, x, y, z)` order.
    Quaternion(crate::math::Quat),
}

impl Rotation {
    /// Normalise to radians-Euler, which is what the wire format carries.
    pub fn to_euler(self) -> Vec3 {
        match self {
            Rotation::Euler(v) => v,
            Rotation::Degrees(v) => Vec3::new(v.x.to_radians(), v.y.to_radians(), v.z.to_radians()),
            Rotation::Quaternion(q) => quat_to_euler_xyz(q),
        }
    }
}

impl Finite for Rotation {
    fn check_finite(&self, field: &str) -> Result<()> {
        match self {
            Rotation::Euler(v) | Rotation::Degrees(v) => v.check_finite(field),
            Rotation::Quaternion(q) => q.check_finite(field),
        }
    }
}

/// Convert a quaternion to XYZ Euler angles in radians.
///
/// Blender's own `Quaternion.to_euler()` uses the same convention; doing it
/// here means a caller can send quaternions without the bridge needing a
/// separate code path.
pub fn quat_to_euler_xyz(q: crate::math::Quat) -> Vec3 {
    let (w, x, y, z) = (q.w, q.x, q.y, q.z);
    let sinp = 2.0 * (w * y - z * x);
    if sinp.abs() >= 1.0 - 1e-9 {
        // Gimbal lock: roll and yaw are degenerate, fold everything into roll.
        let pitch = std::f64::consts::FRAC_PI_2.copysign(sinp);
        let roll = 2.0 * x.atan2(w);
        return Vec3::new(roll, pitch, 0.0);
    }
    let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
    let pitch = sinp.asin();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    Vec3::new(roll, pitch, yaw)
}

/// `object.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateObject {
    /// What to create.
    #[serde(rename = "type")]
    pub primitive: PrimitiveType,
    /// Name for the new object. Blender may append `.001` on collision; the
    /// response reports the name that was actually used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Rotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Vec3>,
    /// Target size along each axis, applied after creation. Mutually exclusive
    /// with `scale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Vec3>,
    /// Collection to link the object into. Defaults to the scene collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    /// Primitive-specific construction parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<PrimitiveOptions>,
}

/// Construction parameters that only apply to some primitives. Unknown-to-the
/// -primitive fields are ignored rather than rejected, because a caller
/// building several primitives from one template should not have to strip them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveOptions {
    /// Radius for spheres, cylinders, cones and torus major radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// Depth/height for cylinders and cones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    /// Size for cubes and planes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Ring/segment counts for spheres and cylinders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rings: Option<u32>,
    /// Icosphere subdivision level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdivisions: Option<u32>,
    /// Torus minor radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minor_radius: Option<f64>,
    /// Cone tip radius; non-zero produces a frustum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius_top: Option<f64>,
    /// Body text for `TEXT` objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Display style for `EMPTY` objects (`PLAIN_AXES`, `ARROWS`, `SPHERE`...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_display_type: Option<String>,
}

impl Validate for PrimitiveOptions {
    fn validate(&self) -> Result<()> {
        for (value, field) in [
            (self.radius, "options.radius"),
            (self.depth, "options.depth"),
            (self.size, "options.size"),
            (self.minor_radius, "options.minor_radius"),
        ] {
            if let Some(v) = value {
                check_positive(v, field)?;
            }
        }
        if let Some(v) = self.radius_top {
            crate::math::check_non_negative(v, "options.radius_top")?;
        }
        for (value, field) in [
            (self.segments, "options.segments"),
            (self.rings, "options.rings"),
        ] {
            if let Some(v) = value
                && v < 3
            {
                return Err(BlenderError::invalid_argument(format!(
                    "`{field}` must be at least 3, got {v}."
                ))
                .with_detail("field", field));
            }
        }
        if let Some(v) = self.subdivisions
            && v > 8
        {
            // 8 subdivisions is ~1.3M faces; beyond that Blender stalls for
            // minutes and the request times out anyway.
            return Err(BlenderError::invalid_argument(format!(
                "`options.subdivisions` above 8 produces an unusable mesh, got {v}."
            ))
            .with_detail("field", "options.subdivisions")
            .with_detail("max", 8));
        }
        Ok(())
    }
}

impl Validate for CreateObject {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.location.check_finite("location")?;
        if let Some(rotation) = &self.rotation {
            rotation.check_finite("rotation")?;
        }
        if let Some(scale) = self.scale {
            scale.check_finite("scale")?;
        }
        if let Some(dimensions) = self.dimensions {
            dimensions.check_finite("dimensions")?;
            for (v, axis) in [
                (dimensions.x, "x"),
                (dimensions.y, "y"),
                (dimensions.z, "z"),
            ] {
                crate::math::check_non_negative(v, &format!("dimensions.{axis}"))?;
            }
        }
        if self.scale.is_some() && self.dimensions.is_some() {
            return Err(BlenderError::invalid_argument(
                "`scale` and `dimensions` both set; pick one -- dimensions are applied as a scale and the two would fight.",
            ));
        }
        if self.primitive == PrimitiveType::Text
            && self
                .options
                .as_ref()
                .and_then(|o| o.text.as_ref())
                .is_none()
        {
            // Not fatal: Blender defaults to "Text". Nothing to reject.
        }
        if let Some(options) = &self.options {
            options.validate()?;
        }
        Ok(())
    }
}

/// `object.transform` -- absolute or relative placement.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransformObject {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Rotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Vec3>,
    /// Explicit world-space size. Applied after `scale`, overriding it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Vec3>,
    /// When true the values are added to the current transform instead of
    /// replacing it (scale is multiplied).
    #[serde(default)]
    pub relative: bool,
    /// Apply in the parent's space rather than world space.
    #[serde(default)]
    pub local: bool,
}

impl Validate for TransformObject {
    fn validate(&self) -> Result<()> {
        self.location.check_finite("location")?;
        if let Some(rotation) = &self.rotation {
            rotation.check_finite("rotation")?;
        }
        if let Some(scale) = self.scale {
            scale.check_finite("scale")?;
            if !self.relative && (scale.x == 0.0 || scale.y == 0.0 || scale.z == 0.0) {
                return Err(BlenderError::new(
                    crate::ErrorCode::InvalidTransform,
                    "A zero scale component collapses the object and cannot be undone by scaling back up.",
                )
                .with_detail_json("scale", &scale));
            }
        }
        if let Some(dimensions) = self.dimensions {
            dimensions.check_finite("dimensions")?;
        }
        if self.location.is_none()
            && self.rotation.is_none()
            && self.scale.is_none()
            && self.dimensions.is_none()
        {
            return Err(BlenderError::invalid_argument(
                "`object.transform` needs at least one of location, rotation, scale or dimensions.",
            ));
        }
        Ok(())
    }
}

/// Which components `object.transform.apply` bakes into the mesh data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct TransformComponents {
    #[serde(default)]
    pub location: bool,
    #[serde(default)]
    pub rotation: bool,
    #[serde(default = "crate::object::default_true")]
    pub scale: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

impl Default for TransformComponents {
    fn default() -> Self {
        Self {
            location: false,
            rotation: false,
            scale: true,
        }
    }
}

/// `object.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListObjects {
    /// Case-insensitive substring match on the object name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Restrict to these object types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<ObjectType>,
    /// Restrict to members of this collection (including nested ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    /// Only currently selected objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Only objects visible in the viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Only objects using this material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialRef>,
    /// Only objects carrying a modifier of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_modifier: Option<String>,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListObjects {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// A single object as reported by `object.get` / `object.list`.
///
/// This is the shape the scene cache stores, so it stays deliberately small:
/// no vertex data, no full modifier settings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ObjectSummary {
    pub id: ObjectId,
    pub name: String,
    #[serde(rename = "type")]
    pub object_type: ObjectType,
    pub location: Vec3,
    /// XYZ Euler in radians.
    pub rotation_euler: Vec3,
    pub scale: Vec3,
    pub dimensions: Vec3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ObjectId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<ModifierSummary>,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub selected: bool,
    /// Present for mesh objects only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<MeshCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<AnimationSummary>,
}

/// Enough of a modifier to answer "what is on this object" without a round trip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModifierSummary {
    pub name: String,
    #[serde(rename = "type")]
    pub modifier_type: String,
    #[serde(default)]
    pub show_viewport: bool,
    #[serde(default)]
    pub show_render: bool,
}

/// Cheap mesh statistics. Counting these is O(1) in Blender.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
pub struct MeshCounts {
    pub vertices: u64,
    pub edges: u64,
    pub faces: u64,
    #[serde(default)]
    pub triangles: u64,
    /// Bumped whenever topology changes, so callers can detect stale indices.
    #[serde(default)]
    pub revision: u64,
}

/// Whether an object is animated, without listing every keyframe.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnimationSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub fcurve_count: u32,
    #[serde(default)]
    pub keyframe_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(f64, f64)>,
}

/// Parent-setting behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ParentType {
    /// Standard object parenting.
    Object,
    /// Parent to a bone of an armature.
    Bone,
    /// Deform via armature modifier.
    Armature,
    /// Parent to a vertex.
    Vertex,
}

/// `object.set_parent`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetParent {
    pub object: ObjectRef,
    pub parent: ObjectRef,
    #[serde(default = "default_parent_type")]
    pub parent_type: ParentType,
    /// Bone name, required when `parent_type` is `BONE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// Preserve the child's current world transform.
    #[serde(default = "default_true")]
    pub keep_transform: bool,
}

fn default_parent_type() -> ParentType {
    ParentType::Object
}

impl Validate for SetParent {
    fn validate(&self) -> Result<()> {
        if self.object == self.parent {
            return Err(BlenderError::invalid_argument(
                "An object cannot be its own parent.",
            ));
        }
        if self.parent_type == ParentType::Bone && self.bone.is_none() {
            return Err(BlenderError::invalid_argument(
                "`bone` is required when `parent_type` is `BONE`.",
            )
            .with_detail("field", "bone"));
        }
        Ok(())
    }
}

/// How `object.origin.set` places the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OriginMode {
    GeometryToOrigin,
    OriginToGeometry,
    OriginToCursor,
    OriginToCenterOfMass,
    OriginToBoundsCenter,
    OriginToBoundsBottom,
    /// Place the origin at an explicit world-space point.
    OriginToPoint,
}

/// `object.origin.set`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetOrigin {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    pub mode: OriginMode,
    /// Required when `mode` is `ORIGIN_TO_POINT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<Vec3>,
}

impl Validate for SetOrigin {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        match (self.mode, self.point) {
            (OriginMode::OriginToPoint, None) => Err(BlenderError::invalid_argument(
                "`point` is required when `mode` is `ORIGIN_TO_POINT`.",
            )
            .with_detail("field", "point")),
            (OriginMode::OriginToPoint, Some(p)) => p.check_finite("point"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_and_dimensions_are_mutually_exclusive() {
        let mut params = CreateObject {
            primitive: PrimitiveType::Cube,
            name: None,
            location: None,
            rotation: None,
            scale: Some(Vec3::ONE),
            dimensions: Some(Vec3::ONE),
            collection: None,
            options: None,
        };
        assert!(params.validate().is_err());
        params.dimensions = None;
        assert!(params.validate().is_ok());
    }

    #[test]
    fn zero_scale_is_rejected_for_absolute_transforms() {
        let params = TransformObject {
            object: ObjectRef::name("Cube"),
            location: None,
            rotation: None,
            scale: Some(Vec3::new(1.0, 0.0, 1.0)),
            dimensions: None,
            relative: false,
            local: false,
        };
        assert_eq!(
            params.validate().unwrap_err().code,
            crate::ErrorCode::InvalidTransform
        );
    }

    #[test]
    fn empty_transform_is_rejected() {
        let params = TransformObject {
            object: ObjectRef::name("Cube"),
            location: None,
            rotation: None,
            scale: None,
            dimensions: None,
            relative: false,
            local: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn degrees_convert_to_radians() {
        let euler = Rotation::Degrees(Vec3::new(90.0, 0.0, 0.0)).to_euler();
        assert!((euler.x - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn identity_quaternion_is_zero_euler() {
        let euler = Rotation::Quaternion(crate::math::Quat::IDENTITY).to_euler();
        assert!(euler.length() < 1e-12);
    }

    #[test]
    fn object_cannot_parent_itself() {
        let params = SetParent {
            object: ObjectRef::name("Cube"),
            parent: ObjectRef::name("Cube"),
            parent_type: ParentType::Object,
            bone: None,
            keep_transform: true,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn rotation_is_externally_tagged_for_clear_schemas() {
        let json = serde_json::to_value(Rotation::Euler(Vec3::ZERO)).unwrap();
        assert!(json.get("euler").is_some(), "got {json}");
    }
}

/// `object.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetObject {
    pub object: ObjectRef,
}

/// `object.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteObjects {
    pub objects: Vec<ObjectRef>,
    /// Also delete every descendant. Without this, children are left behind
    /// with no parent, which is rarely what was wanted.
    #[serde(default)]
    pub delete_children: bool,
    /// Remove the underlying mesh/curve/camera data when nothing else uses it.
    #[serde(default = "default_true")]
    pub delete_data: bool,
}

/// `object.duplicate`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DuplicateObjects {
    pub objects: Vec<ObjectRef>,
    /// Share the source mesh data instead of copying it.
    #[serde(default)]
    pub linked: bool,
    /// How many copies of each object to make.
    #[serde(default = "one_copy")]
    pub count: u32,
    /// Offset applied to copy `n` as `offset * n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_prefix: Option<String>,
    /// Collection for the copies. Defaults to whichever collection the source
    /// is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
}

fn one_copy() -> u32 {
    1
}

/// `object.rename`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameObject {
    pub object: ObjectRef,
    pub name: String,
    /// Rename the attached data-block to match.
    #[serde(default)]
    pub rename_data: bool,
}

/// `object.hide` / `object.show`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VisibilityUpdate {
    pub objects: Vec<ObjectRef>,
    /// Affect viewport visibility.
    #[serde(default = "default_true")]
    pub viewport: bool,
    /// Affect render visibility.
    #[serde(default = "default_true")]
    pub render: bool,
}

/// `object.set_display`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DisplayUpdate {
    pub objects: Vec<ObjectRef>,
    /// `BOUNDS`, `WIRE`, `SOLID` or `TEXTURED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_wire: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_in_front: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_name: Option<bool>,
    /// Viewport display colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<crate::math::Color4>,
}

/// `object.transform.apply`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplyTransforms {
    pub objects: Vec<ObjectRef>,
    #[serde(default)]
    pub location: bool,
    #[serde(default)]
    pub rotation: bool,
    #[serde(default = "default_true")]
    pub scale: bool,
}

/// `object.join`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JoinObjects {
    /// The object everything is merged into. It survives; the sources do not.
    pub target: ObjectRef,
    pub sources: Vec<ObjectRef>,
}

/// `object.separate`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SeparateObject {
    pub object: ObjectRef,
    /// `LOOSE`, `MATERIAL` or `SELECTED`.
    #[serde(default = "default_separate_method")]
    pub method: String,
}

fn default_separate_method() -> String {
    "LOOSE".to_string()
}

/// `object.convert`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConvertObjects {
    pub objects: Vec<ObjectRef>,
    /// `MESH`, `CURVE`, `CURVES` or `GPENCIL`.
    pub target: String,
    /// Keep the original alongside the converted copy.
    #[serde(default)]
    pub keep_original: bool,
}

impl Validate for GetObject {}
impl Validate for VisibilityUpdate {}
impl Validate for ApplyTransforms {}

impl Validate for DeleteObjects {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        Ok(())
    }
}

impl Validate for DuplicateObjects {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        if self.count == 0 || self.count > 1000 {
            return Err(BlenderError::invalid_argument(format!(
                "`count` must be between 1 and 1000, got {}.",
                self.count
            )));
        }
        if let Some(offset) = self.offset {
            offset.check_finite("offset")?;
        }
        if self.count > 1 && self.offset.is_none() {
            return Err(BlenderError::invalid_argument(
                "Duplicating more than once without an `offset` stacks every copy in the same \
                 place. Provide an offset, or duplicate one at a time and place each copy.",
            ));
        }
        Ok(())
    }
}

impl Validate for RenameObject {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")
    }
}

impl Validate for DisplayUpdate {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        if let Some(display_type) = &self.display_type {
            const TYPES: [&str; 4] = ["BOUNDS", "WIRE", "SOLID", "TEXTURED"];
            if !TYPES.contains(&display_type.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "display_type",
                    display_type.clone(),
                    TYPES,
                ));
            }
        }
        if let Some(color) = self.color {
            color.check_finite("color")?;
        }
        Ok(())
    }
}

impl Validate for JoinObjects {
    fn validate(&self) -> Result<()> {
        if self.sources.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`sources` must name at least one object.",
            ));
        }
        if self.sources.contains(&self.target) {
            return Err(BlenderError::invalid_argument(
                "`target` must not appear in `sources`; it is the object that survives.",
            ));
        }
        Ok(())
    }
}

impl Validate for SeparateObject {
    fn validate(&self) -> Result<()> {
        const METHODS: [&str; 3] = ["LOOSE", "MATERIAL", "SELECTED"];
        if !METHODS.contains(&self.method.as_str()) {
            return Err(BlenderError::invalid_enum(
                "method",
                self.method.clone(),
                METHODS,
            ));
        }
        Ok(())
    }
}

impl Validate for ConvertObjects {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        const TARGETS: [&str; 4] = ["MESH", "CURVE", "CURVES", "GPENCIL"];
        if !TARGETS.contains(&self.target.as_str()) {
            return Err(BlenderError::invalid_enum(
                "target",
                self.target.clone(),
                TARGETS,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn stacked_duplicates_are_refused() {
        let params = DuplicateObjects {
            objects: vec![ObjectRef::name("Cube")],
            linked: false,
            count: 5,
            offset: None,
            name_prefix: None,
            collection: None,
        };
        assert!(params.validate().is_err());
        let params = DuplicateObjects {
            offset: Some(Vec3::new(2.0, 0.0, 0.0)),
            ..params
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn join_refuses_a_target_that_is_also_a_source() {
        let params = JoinObjects {
            target: ObjectRef::name("A"),
            sources: vec![ObjectRef::name("A")],
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn convert_targets_are_checked() {
        let params = ConvertObjects {
            objects: vec![ObjectRef::name("Cube")],
            target: "NURBS".into(),
            keep_original: false,
        };
        assert_eq!(
            params.validate().unwrap_err().code,
            crate::ErrorCode::InvalidEnum
        );
    }
}

/// `object.clear_parent`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClearParent {
    pub objects: Vec<ObjectRef>,
    /// Leave the objects where they appear on screen rather than snapping them
    /// back to their unparented local transform.
    #[serde(default = "default_true")]
    pub keep_transform: bool,
}

impl Validate for ClearParent {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        Ok(())
    }
}

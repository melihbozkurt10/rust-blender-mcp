//! Small, dependency-free math types shared by the protocol and the domain
//! layer.
//!
//! Everything here is finite-checked on the way in: `NaN` and infinities never
//! reach Blender, because a single non-finite transform component silently
//! corrupts a whole object hierarchy.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{BlenderError, ErrorCode};

/// A 2D vector (UV offsets, node editor coordinates, sensor sizes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

/// A 3D vector: locations, scales, Euler rotations (radians), directions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// A quaternion in Blender's `(w, x, y, z)` ordering.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Quat {
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Linear RGBA colour. Blender works in scene-linear space; values above 1.0
/// are legal and meaningful for emission.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Color4 {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    #[serde(default = "one")]
    pub a: f64,
}

fn one() -> f64 {
    1.0
}

/// Axis-aligned bounding box in world space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

/// A principal axis, with sign. Used by mirroring, array offsets and turntables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Axis {
    X,
    Y,
    Z,
    NegX,
    NegY,
    NegZ,
}

impl Axis {
    pub const fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::new(1.0, 0.0, 0.0),
            Axis::Y => Vec3::new(0.0, 1.0, 0.0),
            Axis::Z => Vec3::new(0.0, 0.0, 1.0),
            Axis::NegX => Vec3::new(-1.0, 0.0, 0.0),
            Axis::NegY => Vec3::new(0.0, -1.0, 0.0),
            Axis::NegZ => Vec3::new(0.0, 0.0, -1.0),
        }
    }

    /// The unsigned axis letter, which is what most `bpy` enums expect.
    pub const fn letter(self) -> &'static str {
        match self {
            Axis::X | Axis::NegX => "X",
            Axis::Y | Axis::NegY => "Y",
            Axis::Z | Axis::NegZ => "Z",
        }
    }

    pub const fn is_negative(self) -> bool {
        matches!(self, Axis::NegX | Axis::NegY | Axis::NegZ)
    }
}

/// Anything whose numeric components must be finite before crossing the wire.
pub trait Finite {
    /// `field` names the argument being checked, so the error tells the caller
    /// exactly which input to fix.
    fn check_finite(&self, field: &str) -> Result<(), BlenderError>;
}

fn finite(value: f64, field: &str, component: &str) -> Result<(), BlenderError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(BlenderError::new(
            ErrorCode::InvalidTransform,
            format!("`{field}.{component}` must be a finite number, got {value}"),
        )
        .with_detail("field", field)
        .with_detail("component", component))
    }
}

impl Vec2 {
    pub const ZERO: Self = Self::new(0.0, 0.0);
    pub const ONE: Self = Self::new(1.0, 1.0);

    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub const fn splat(v: f64) -> Self {
        Self::new(v, v)
    }

    pub fn to_array(self) -> [f64; 2] {
        [self.x, self.y]
    }
}

impl Finite for Vec2 {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        finite(self.x, field, "x")?;
        finite(self.y, field, "y")
    }
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);
    pub const ONE: Self = Self::new(1.0, 1.0, 1.0);
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn splat(v: f64) -> Self {
        Self::new(v, v, v)
    }

    pub fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    pub fn component_mul(self, o: Self) -> Self {
        Self::new(self.x * o.x, self.y * o.y, self.z * o.z)
    }

    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    pub fn distance(self, o: Self) -> f64 {
        (self - o).length()
    }

    /// Returns `None` for a zero-length vector rather than producing `NaN`.
    pub fn normalized(self) -> Option<Self> {
        let len = self.length();
        if len <= f64::EPSILON {
            None
        } else {
            Some(self * (1.0 / len))
        }
    }

    pub fn lerp(self, o: Self, t: f64) -> Self {
        self + (o - self) * t
    }

    pub fn max_component(self) -> f64 {
        self.x.max(self.y).max(self.z)
    }

    pub fn min_component(self) -> f64 {
        self.x.min(self.y).min(self.z)
    }

    /// Euler XYZ rotation that points an object's local `-Z` at `target` with
    /// local `+Y` up, which is the convention Blender cameras and spot lights
    /// use.
    pub fn look_at_euler(self, target: Self) -> Vec3 {
        // With Blender's XYZ Euler order the composed rotation is Rz * Ry * Rx,
        // so an euler of (pitch, 0, yaw) sends local -Z to
        //   (-sin(pitch)sin(yaw), sin(pitch)cos(yaw), -cos(pitch)).
        // Inverting that gives the two angles directly, with roll left at zero.
        let Some(dir) = (target - self).normalized() else {
            // Degenerate: the target is the eye. Any rotation is as good as
            // another, so keep the object where it was.
            return Vec3::ZERO;
        };
        let pitch = (-dir.z).clamp(-1.0, 1.0).acos();
        // atan2(0, 0) is 0, which is the stable choice when looking straight
        // up or down and the yaw is genuinely arbitrary.
        let yaw = (-dir.x).atan2(dir.y);
        Vec3::new(pitch, 0.0, yaw)
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl std::ops::Mul<f64> for Vec3 {
    type Output = Self;
    fn mul(self, s: f64) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

impl std::ops::Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl Finite for Vec3 {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        finite(self.x, field, "x")?;
        finite(self.y, field, "y")?;
        finite(self.z, field, "z")
    }
}

impl Quat {
    pub const IDENTITY: Self = Self {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    pub const fn new(w: f64, x: f64, y: f64, z: f64) -> Self {
        Self { w, x, y, z }
    }

    pub fn to_array(self) -> [f64; 4] {
        [self.w, self.x, self.y, self.z]
    }

    pub fn from_axis_angle(axis: Vec3, radians: f64) -> Option<Self> {
        let axis = axis.normalized()?;
        let (s, c) = (radians * 0.5).sin_cos();
        Some(Self::new(c, axis.x * s, axis.y * s, axis.z * s))
    }
}

impl Finite for Quat {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        finite(self.w, field, "w")?;
        finite(self.x, field, "x")?;
        finite(self.y, field, "y")?;
        finite(self.z, field, "z")
    }
}

impl Color4 {
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);

    pub const fn rgb(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    pub fn to_array(self) -> [f64; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Approximate blackbody colour for a temperature in Kelvin, normalised so
    /// the brightest channel is 1.0. Good enough for lighting rigs; Cycles has
    /// a proper blackbody node when accuracy matters.
    pub fn from_kelvin(kelvin: f64) -> Self {
        let t = (kelvin.clamp(1000.0, 40000.0)) / 100.0;
        let r = if t <= 66.0 {
            255.0
        } else {
            329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
        };
        let g = if t <= 66.0 {
            99.470_802_586 * t.ln() - 161.119_568_166
        } else {
            288.122_169_528 * (t - 60.0).powf(-0.075_514_849_2)
        };
        let b = if t >= 66.0 {
            255.0
        } else if t <= 19.0 {
            0.0
        } else {
            138.517_731_223 * (t - 10.0).ln() - 305.044_792_730
        };
        let scale = 1.0 / 255.0;
        Self::rgb(
            (r * scale).clamp(0.0, 1.0),
            (g * scale).clamp(0.0, 1.0),
            (b * scale).clamp(0.0, 1.0),
        )
    }
}

impl Finite for Color4 {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        finite(self.r, field, "r")?;
        finite(self.g, field, "g")?;
        finite(self.b, field, "b")?;
        finite(self.a, field, "a")
    }
}

impl Aabb {
    pub const fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut bb = Self::new(first, first);
        for p in iter {
            bb.expand(p);
        }
        Some(bb)
    }

    pub fn expand(&mut self, p: Vec3) {
        self.min = Vec3::new(
            self.min.x.min(p.x),
            self.min.y.min(p.y),
            self.min.z.min(p.z),
        );
        self.max = Vec3::new(
            self.max.x.max(p.x),
            self.max.y.max(p.y),
            self.max.z.max(p.z),
        );
    }

    pub fn union(self, other: Self) -> Self {
        let mut out = self;
        out.expand(other.min);
        out.expand(other.max);
        out
    }

    pub fn center(self) -> Vec3 {
        self.min.lerp(self.max, 0.5)
    }

    pub fn size(self) -> Vec3 {
        self.max - self.min
    }

    /// Radius of the sphere that encloses the box.
    pub fn bounding_radius(self) -> f64 {
        (self.size() * 0.5).length()
    }

    pub fn corners(self) -> [Vec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(a.x, b.y, b.z),
            Vec3::new(b.x, b.y, b.z),
        ]
    }
}

impl Finite for Aabb {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        self.min.check_finite(&format!("{field}.min"))?;
        self.max.check_finite(&format!("{field}.max"))
    }
}

impl<T: Finite> Finite for Option<T> {
    fn check_finite(&self, field: &str) -> Result<(), BlenderError> {
        match self {
            Some(v) => v.check_finite(field),
            None => Ok(()),
        }
    }
}

/// Guard a scalar that must be finite.
pub fn check_scalar_finite(value: f64, field: &str) -> Result<(), BlenderError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(BlenderError::new(
            ErrorCode::InvalidArgument,
            format!("`{field}` must be a finite number, got {value}"),
        )
        .with_detail("field", field))
    }
}

/// Guard a scalar that must be finite and inside `[min, max]`.
pub fn check_range(value: f64, min: f64, max: f64, field: &str) -> Result<(), BlenderError> {
    check_scalar_finite(value, field)?;
    if value < min || value > max {
        return Err(BlenderError::new(
            ErrorCode::InvalidArgument,
            format!("`{field}` must be between {min} and {max}, got {value}"),
        )
        .with_detail("field", field)
        .with_detail("min", min)
        .with_detail("max", max));
    }
    Ok(())
}

/// Guard a scalar that must be finite and strictly positive.
pub fn check_positive(value: f64, field: &str) -> Result<(), BlenderError> {
    check_scalar_finite(value, field)?;
    if value <= 0.0 {
        return Err(BlenderError::new(
            ErrorCode::InvalidArgument,
            format!("`{field}` must be greater than zero, got {value}"),
        )
        .with_detail("field", field));
    }
    Ok(())
}

/// Guard a scalar that must be finite and non-negative.
pub fn check_non_negative(value: f64, field: &str) -> Result<(), BlenderError> {
    check_scalar_finite(value, field)?;
    if value < 0.0 {
        return Err(BlenderError::new(
            ErrorCode::InvalidArgument,
            format!("`{field}` must not be negative, got {value}"),
        )
        .with_detail("field", field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_finite_components() {
        let bad = Vec3::new(1.0, f64::NAN, 0.0);
        let err = bad.check_finite("location").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidTransform);
        assert!(err.message.contains("location.y"));
    }

    /// Apply an XYZ Euler (Rz * Ry * Rx) to a vector, mirroring Blender.
    fn rotate_xyz(euler: Vec3, v: Vec3) -> Vec3 {
        let (sx, cx) = euler.x.sin_cos();
        let (sy, cy) = euler.y.sin_cos();
        let (sz, cz) = euler.z.sin_cos();
        let after_x = Vec3::new(v.x, v.y * cx - v.z * sx, v.y * sx + v.z * cx);
        let after_y = Vec3::new(
            after_x.x * cy + after_x.z * sy,
            after_x.y,
            -after_x.x * sy + after_x.z * cy,
        );
        Vec3::new(
            after_y.x * cz - after_y.y * sz,
            after_y.x * sz + after_y.y * cz,
            after_y.z,
        )
    }

    #[test]
    fn look_at_points_local_negative_z_at_the_target() {
        // An object straight above the origin needs no rotation at all: its
        // local -Z already points down.
        let euler = Vec3::new(0.0, 0.0, 10.0).look_at_euler(Vec3::ZERO);
        assert!(
            euler.length() < 1e-9,
            "expected identity rotation, got {euler:?}"
        );

        // For arbitrary placements, rotating local -Z by the result must
        // reproduce the direction to the target.
        for eye in [
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(-4.0, 7.0, 2.0),
            Vec3::new(3.0, -3.0, -5.0),
            Vec3::new(0.0, 0.0, -6.0),
        ] {
            let target = Vec3::new(1.0, 2.0, 0.5);
            let euler = eye.look_at_euler(target);
            let forward = rotate_xyz(euler, Vec3::new(0.0, 0.0, -1.0));
            let expected = (target - eye).normalized().unwrap();
            assert!(
                forward.distance(expected) < 1e-9,
                "eye {eye:?}: forward {forward:?} != expected {expected:?}"
            );
        }
    }

    #[test]
    fn look_at_is_stable_when_target_equals_eye() {
        assert_eq!(Vec3::ZERO.look_at_euler(Vec3::ZERO), Vec3::ZERO);
    }

    #[test]
    fn aabb_bounds_are_symmetric() {
        let bb =
            Aabb::from_points([Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0)]).unwrap();
        assert_eq!(bb.center(), Vec3::ZERO);
        assert_eq!(bb.size(), Vec3::new(2.0, 4.0, 6.0));
    }

    #[test]
    fn kelvin_is_warm_below_daylight() {
        let warm = Color4::from_kelvin(2700.0);
        let cool = Color4::from_kelvin(8000.0);
        assert!(warm.r > warm.b);
        assert!(cool.b > cool.r);
    }
}

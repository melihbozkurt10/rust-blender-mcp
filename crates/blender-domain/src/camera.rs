//! Camera framing.
//!
//! Fitting a subject in frame is a closed-form calculation, not something to
//! iterate towards by rendering and looking. Given the subject's bounds and the
//! camera's field of view, the distance that just contains it is one equation.

use blender_protocol::{
    BlenderError, Result,
    math::{Aabb, Vec3, check_range},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a camera needs to know to frame something.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct FramingRequest {
    /// World-space bounds of everything that must be in frame.
    pub bounds: Aabb,
    /// Horizontal field of view in radians.
    pub horizontal_fov: f64,
    /// Render aspect ratio, width over height.
    pub aspect: f64,
    /// Extra room around the subject, as a fraction of its size.
    pub padding: f64,
    /// Direction to view from, in world space. Normalised internally.
    pub direction: Vec3,
}

/// Where the camera should stand.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FramingResult {
    pub location: Vec3,
    /// XYZ Euler in radians, aiming the camera at the subject.
    pub rotation_euler: Vec3,
    /// Centre of the subject, which is also the focus point.
    pub target: Vec3,
    /// How far the camera ended up from the subject centre.
    pub distance: f64,
    /// Radius of the sphere enclosing the subject.
    pub radius: f64,
    /// Orthographic view height that would frame the same subject.
    pub ortho_scale: f64,
}

/// The three-quarter view a product shot conventionally uses: front-left and
/// slightly above. Far more informative than an axis-aligned view, and it is
/// what someone means when they say "show me the model".
pub const DEFAULT_DIRECTION: Vec3 = Vec3::new(-0.6, -0.7, 0.38);

impl FramingRequest {
    pub fn new(bounds: Aabb, horizontal_fov: f64, aspect: f64) -> Self {
        Self {
            bounds,
            horizontal_fov,
            aspect,
            padding: 0.1,
            direction: DEFAULT_DIRECTION,
        }
    }

    /// Work out where to stand.
    pub fn solve(&self) -> Result<FramingResult> {
        check_range(
            self.horizontal_fov,
            1e-3,
            std::f64::consts::PI - 1e-3,
            "horizontal_fov",
        )?;
        check_range(self.aspect, 1e-3, 1000.0, "aspect")?;
        check_range(self.padding, 0.0, 100.0, "padding")?;

        let target = self.bounds.center();
        let radius = self.bounds.bounding_radius().max(1e-6);

        // The vertical field of view follows from the horizontal one and the
        // aspect ratio; fitting the bounding sphere in the *tighter* of the two
        // is what guarantees nothing is cropped in either dimension.
        let vertical_fov = 2.0 * ((self.horizontal_fov * 0.5).tan() / self.aspect).atan();
        let tightest = self.horizontal_fov.min(vertical_fov);
        let half = (tightest * 0.5).sin().max(1e-6);
        let distance = (radius * (1.0 + self.padding)) / half;

        let direction = self.direction.normalized().ok_or_else(|| {
            BlenderError::invalid_argument(
                "`direction` has zero length, so it does not describe a direction.",
            )
        })?;

        let location = target + direction * distance;
        Ok(FramingResult {
            location,
            rotation_euler: location.look_at_euler(target),
            target,
            distance,
            radius,
            ortho_scale: 2.0 * radius * (1.0 + self.padding),
        })
    }
}

/// The focal length that frames a subject from a fixed distance.
///
/// The other half of the problem: when the camera cannot move, the lens has to
/// do the work.
pub fn focal_length_to_fit(
    radius: f64,
    distance: f64,
    sensor_width_mm: f64,
    padding: f64,
) -> Result<f64> {
    if radius <= 0.0 || distance <= radius {
        return Err(BlenderError::invalid_argument(
            "The camera is inside the subject's bounding sphere; no lens frames that.",
        )
        .with_detail("radius", radius)
        .with_detail("distance", distance));
    }
    let padded = radius * (1.0 + padding.max(0.0));
    let half_angle = (padded / distance).asin();
    Ok((sensor_width_mm * 0.5) / half_angle.tan())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube() -> Aabb {
        Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0))
    }

    #[test]
    fn a_wider_lens_stands_closer() {
        let wide = FramingRequest::new(unit_cube(), 1.2, 16.0 / 9.0)
            .solve()
            .unwrap();
        let narrow = FramingRequest::new(unit_cube(), 0.4, 16.0 / 9.0)
            .solve()
            .unwrap();
        assert!(
            wide.distance < narrow.distance,
            "wide {} should be nearer than narrow {}",
            wide.distance,
            narrow.distance
        );
    }

    #[test]
    fn padding_pushes_the_camera_back_proportionally() {
        let mut request = FramingRequest::new(unit_cube(), 0.8, 1.0);
        let tight = request.solve().unwrap();
        request.padding = 1.0;
        let loose = request.solve().unwrap();
        assert!((loose.distance / tight.distance - (2.0 / 1.1)).abs() < 1e-9);
    }

    #[test]
    fn the_tighter_axis_decides_the_distance() {
        // For a fixed *horizontal* field of view, a 16:9 frame is much
        // narrower vertically than a 9:16 one, so it is the vertical axis that
        // crops first and the camera has to back off further. Fitting the
        // tighter of the two angles is exactly what stops the subject being
        // cut off at the top and bottom.
        let landscape = FramingRequest::new(unit_cube(), 1.0, 16.0 / 9.0)
            .solve()
            .unwrap();
        let portrait = FramingRequest::new(unit_cube(), 1.0, 9.0 / 16.0)
            .solve()
            .unwrap();
        assert!(
            landscape.distance > portrait.distance,
            "landscape {} should need more room than portrait {} at the same horizontal FOV",
            landscape.distance,
            portrait.distance
        );

        // A square frame sits between the two, because neither axis is tighter.
        let square = FramingRequest::new(unit_cube(), 1.0, 1.0).solve().unwrap();
        assert!(square.distance <= landscape.distance);
        assert!(square.distance >= portrait.distance);
    }

    #[test]
    fn the_camera_ends_up_at_the_computed_distance() {
        let result = FramingRequest::new(unit_cube(), 0.9, 1.5).solve().unwrap();
        let actual = result.location.distance(result.target);
        assert!((actual - result.distance).abs() < 1e-9);
    }

    #[test]
    fn the_camera_looks_at_the_subject() {
        let result = FramingRequest::new(unit_cube(), 0.9, 1.5).solve().unwrap();
        // Reproduce the forward vector from the euler and check it points home.
        let euler = result.rotation_euler;
        let (sx, cx) = euler.x.sin_cos();
        let (sz, cz) = euler.z.sin_cos();
        let forward = Vec3::new(-sx * sz, sx * cz, -cx);
        let expected = (result.target - result.location).normalized().unwrap();
        assert!(
            forward.distance(expected) < 1e-9,
            "{forward:?} vs {expected:?}"
        );
    }

    #[test]
    fn an_offset_subject_is_still_centred() {
        let bounds = Aabb::new(Vec3::new(9.0, 9.0, 9.0), Vec3::new(11.0, 11.0, 11.0));
        let result = FramingRequest::new(bounds, 1.0, 1.0).solve().unwrap();
        assert_eq!(result.target, Vec3::splat(10.0));
    }

    #[test]
    fn a_zero_direction_is_refused() {
        let mut request = FramingRequest::new(unit_cube(), 1.0, 1.0);
        request.direction = Vec3::ZERO;
        assert!(request.solve().is_err());
    }

    #[test]
    fn focal_length_grows_as_the_subject_shrinks() {
        let near = focal_length_to_fit(1.0, 10.0, 36.0, 0.0).unwrap();
        let far = focal_length_to_fit(0.5, 10.0, 36.0, 0.0).unwrap();
        assert!(far > near, "a smaller subject needs a longer lens");
    }

    #[test]
    fn a_camera_inside_the_subject_is_refused() {
        assert!(focal_length_to_fit(5.0, 1.0, 36.0, 0.0).is_err());
    }

    #[test]
    fn framing_and_focal_length_agree() {
        // Solve for distance at a given field of view, then solve for the lens
        // at that distance: the two must describe the same shot.
        let sensor = 36.0;
        let fov = 2.0 * ((sensor * 0.5) / 50.0_f64).atan();
        let framed = FramingRequest::new(unit_cube(), fov, 1.0).solve().unwrap();
        let lens = focal_length_to_fit(framed.radius, framed.distance, sensor, 0.1).unwrap();
        assert!((lens - 50.0).abs() < 1e-6, "expected 50mm, got {lens}");
    }
}

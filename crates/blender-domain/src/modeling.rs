//! Parametric modelling.
//!
//! A wall between two points is the smallest complete demonstration of the
//! architecture: the caller states intent, the geometry is worked out here, and
//! Blender is told to make a cube of a certain size at a certain place with a
//! certain rotation. No Python is generated, and the maths is testable without
//! Blender running.

use blender_protocol::{
    BlenderError, Result,
    math::{Vec3, check_positive},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A wall described by its two ends.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WallSpec {
    /// One end of the wall, at its base.
    pub start: Vec3,
    /// The other end, at its base.
    pub end: Vec3,
    /// How tall the wall stands above its base.
    pub height: f64,
    /// How thick the wall is, measured across its length.
    pub thickness: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Centre the wall on the line between the points rather than growing it
    /// upward from them.
    #[serde(default)]
    pub centred_vertically: bool,
}

/// Where a wall goes and how big it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WallPlacement {
    /// Centre of the wall in world space, which is where the object origin goes.
    pub location: Vec3,
    /// XYZ Euler rotation in radians. Only Z is non-zero: walls stand upright.
    pub rotation_euler: Vec3,
    /// Size along the wall's own axes: length, thickness, height.
    pub dimensions: Vec3,
    /// Length of the wall, which is also `dimensions.x`.
    pub length: f64,
}

impl WallSpec {
    /// Work out where the wall goes.
    ///
    /// A cube primitive is 2 units across, so the caller sets `dimensions`
    /// rather than a scale and lets the bridge derive the scale from the
    /// object's own bounds -- that keeps this function independent of what
    /// primitive is eventually used.
    pub fn plan(&self) -> Result<WallPlacement> {
        self.start.check_finite_named("start")?;
        self.end.check_finite_named("end")?;
        check_positive(self.height, "height")?;
        check_positive(self.thickness, "thickness")?;

        let along = self.end - self.start;
        let length = (along.x * along.x + along.y * along.y).sqrt();
        if length < 1e-6 {
            return Err(BlenderError::invalid_argument(
                "`start` and `end` are the same point, so the wall would have no length.",
            )
            .with_detail_json("start", &self.start)
            .with_detail_json("end", &self.end));
        }

        // Walls are vertical: the line between the points sets the footprint,
        // and any height difference between the ends is ignored rather than
        // silently producing a leaning wall.
        let yaw = along.y.atan2(along.x);
        let midpoint = self.start.lerp(self.end, 0.5);
        let base_z = midpoint.z;
        let centre_z = if self.centred_vertically {
            base_z
        } else {
            base_z + self.height * 0.5
        };

        Ok(WallPlacement {
            location: Vec3::new(midpoint.x, midpoint.y, centre_z),
            rotation_euler: Vec3::new(0.0, 0.0, yaw),
            dimensions: Vec3::new(length, self.thickness, self.height),
            length,
        })
    }
}

/// Convenience so the checks above read as prose.
trait CheckFiniteNamed {
    fn check_finite_named(&self, field: &str) -> Result<()>;
}

impl CheckFiniteNamed for Vec3 {
    fn check_finite_named(&self, field: &str) -> Result<()> {
        blender_protocol::math::Finite::check_finite(self, field)
    }
}

/// A run of walls forming a closed or open outline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WallRun {
    /// Corner points, in order.
    pub points: Vec<Vec3>,
    pub height: f64,
    pub thickness: f64,
    /// Join the last point back to the first.
    #[serde(default)]
    pub closed: bool,
}

impl WallRun {
    /// One placement per segment.
    pub fn plan(&self) -> Result<Vec<WallPlacement>> {
        if self.points.len() < 2 {
            return Err(BlenderError::invalid_argument(
                "A wall run needs at least two points.",
            ));
        }
        let mut segments: Vec<(Vec3, Vec3)> = self
            .points
            .windows(2)
            .map(|pair| (pair[0], pair[1]))
            .collect();
        if self.closed {
            segments.push((*self.points.last().unwrap(), self.points[0]));
        }

        segments
            .into_iter()
            .map(|(start, end)| {
                WallSpec {
                    start,
                    end,
                    height: self.height,
                    thickness: self.thickness,
                    name: None,
                    centred_vertically: false,
                }
                .plan()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(start: Vec3, end: Vec3) -> WallSpec {
        WallSpec {
            start,
            end,
            height: 3.0,
            thickness: 0.2,
            name: None,
            centred_vertically: false,
        }
    }

    #[test]
    fn a_wall_along_x_needs_no_rotation() {
        let plan = wall(Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0)).plan().unwrap();
        assert!((plan.rotation_euler.z).abs() < 1e-12);
        assert_eq!(plan.dimensions, Vec3::new(5.0, 0.2, 3.0));
        // Centred along its length, and standing on its base.
        assert_eq!(plan.location, Vec3::new(2.5, 0.0, 1.5));
    }

    #[test]
    fn a_wall_along_y_is_rotated_ninety_degrees() {
        let plan = wall(Vec3::ZERO, Vec3::new(0.0, 4.0, 0.0)).plan().unwrap();
        assert!(
            (plan.rotation_euler.z - std::f64::consts::FRAC_PI_2).abs() < 1e-12,
            "got {}",
            plan.rotation_euler.z
        );
        assert!((plan.length - 4.0).abs() < 1e-12);
    }

    #[test]
    fn a_diagonal_wall_has_the_right_length_and_angle() {
        let plan = wall(Vec3::ZERO, Vec3::new(3.0, 4.0, 0.0)).plan().unwrap();
        assert!((plan.length - 5.0).abs() < 1e-12, "3-4-5 triangle");
        assert!((plan.rotation_euler.z - (4.0f64).atan2(3.0)).abs() < 1e-12);
    }

    #[test]
    fn height_differences_do_not_lean_the_wall() {
        // The footprint is what matters; a raised end should not tilt the wall.
        let plan = wall(Vec3::ZERO, Vec3::new(5.0, 0.0, 2.0)).plan().unwrap();
        assert_eq!(plan.rotation_euler.x, 0.0);
        assert_eq!(plan.rotation_euler.y, 0.0);
        assert!(
            (plan.length - 5.0).abs() < 1e-12,
            "length ignores the Z difference"
        );
        assert!(
            (plan.location.z - (1.0 + 1.5)).abs() < 1e-12,
            "base is the midpoint height"
        );
    }

    #[test]
    fn centring_vertically_puts_the_origin_on_the_line() {
        let mut spec = wall(Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0));
        spec.centred_vertically = true;
        assert_eq!(spec.plan().unwrap().location.z, 0.0);
    }

    #[test]
    fn a_zero_length_wall_is_refused() {
        let error = wall(Vec3::ZERO, Vec3::ZERO).plan().unwrap_err();
        assert!(error.message.contains("no length"));
    }

    #[test]
    fn non_positive_dimensions_are_refused() {
        let mut spec = wall(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0));
        spec.height = 0.0;
        assert!(spec.plan().is_err());
        spec.height = 3.0;
        spec.thickness = -1.0;
        assert!(spec.plan().is_err());
    }

    #[test]
    fn a_closed_run_produces_one_segment_per_edge() {
        let run = WallRun {
            points: vec![
                Vec3::ZERO,
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(4.0, 3.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
            ],
            height: 2.5,
            thickness: 0.15,
            closed: true,
        };
        let plans = run.plan().unwrap();
        assert_eq!(plans.len(), 4, "four corners, four walls when closed");
        let perimeter: f64 = plans.iter().map(|p| p.length).sum();
        assert!((perimeter - 14.0).abs() < 1e-9, "got {perimeter}");
    }

    #[test]
    fn an_open_run_has_one_fewer_segment() {
        let run = WallRun {
            points: vec![
                Vec3::ZERO,
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(4.0, 3.0, 0.0),
            ],
            height: 2.5,
            thickness: 0.15,
            closed: false,
        };
        assert_eq!(run.plan().unwrap().len(), 2);
    }
}

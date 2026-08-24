//! Lighting rigs.
//!
//! Three-point lighting is a convention with real geometry behind it: a key at
//! roughly 45 degrees off-axis and above, a fill on the opposite side at a
//! fraction of the key's power, and a rim behind the subject picking out its
//! silhouette. Working the positions out here means the result is the same
//! every time and can be tested without rendering anything.

use blender_protocol::{
    Result,
    light::{AreaShape, LightSettings, LightType},
    math::{Aabb, Vec3, check_non_negative, check_range},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A three-point lighting setup to build.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreePointSpec {
    /// What the rig lights.
    pub subject: Aabb,
    /// Key light power in watts. Scaled with subject size when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_energy: Option<f64>,
    /// Fill power as a fraction of the key. The classic ratio is about a third.
    #[serde(default = "default_fill_ratio")]
    pub fill_ratio: f64,
    /// Rim power as a fraction of the key.
    #[serde(default = "default_rim_ratio")]
    pub rim_ratio: f64,
    /// How far the lights stand, as a multiple of the subject's radius.
    #[serde(default = "default_distance_factor")]
    pub distance_factor: f64,
    /// Key colour temperature in Kelvin.
    #[serde(default = "default_key_temperature")]
    pub key_temperature: f64,
    /// Fill colour temperature. Cooler than the key by convention, which reads
    /// as bounced daylight.
    #[serde(default = "default_fill_temperature")]
    pub fill_temperature: f64,
    /// Rim colour temperature.
    #[serde(default = "default_rim_temperature")]
    pub rim_temperature: f64,
    /// Which way the subject faces, in world space. The key goes to its left.
    #[serde(default = "default_facing")]
    pub facing: Vec3,
}

fn default_fill_ratio() -> f64 {
    0.35
}
fn default_rim_ratio() -> f64 {
    0.6
}
fn default_distance_factor() -> f64 {
    4.0
}
fn default_key_temperature() -> f64 {
    5200.0
}
fn default_fill_temperature() -> f64 {
    6500.0
}
fn default_rim_temperature() -> f64 {
    6200.0
}
fn default_facing() -> Vec3 {
    // -Y is the direction a default Blender camera looks from, so a subject
    // "facing the camera" faces -Y.
    Vec3::new(0.0, -1.0, 0.0)
}

/// One light in the rig.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannedLight {
    /// `Key`, `Fill` or `Rim`.
    pub role: String,
    pub name: String,
    pub light_type: LightType,
    pub location: Vec3,
    /// Point the light at this, rather than a rotation the caller has to
    /// compute.
    pub look_at: Vec3,
    pub settings: LightSettings,
}

/// The whole rig.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThreePointPlan {
    pub lights: Vec<PlannedLight>,
    pub target: Vec3,
    pub radius: f64,
    pub distance: f64,
}

impl ThreePointSpec {
    pub fn plan(&self) -> Result<ThreePointPlan> {
        check_range(self.fill_ratio, 0.0, 4.0, "fill_ratio")?;
        check_range(self.rim_ratio, 0.0, 4.0, "rim_ratio")?;
        check_range(self.distance_factor, 0.5, 100.0, "distance_factor")?;
        for (value, field) in [
            (self.key_temperature, "key_temperature"),
            (self.fill_temperature, "fill_temperature"),
            (self.rim_temperature, "rim_temperature"),
        ] {
            check_range(value, 1000.0, 40000.0, field)?;
        }
        if let Some(energy) = self.key_energy {
            check_non_negative(energy, "key_energy")?;
        }

        let target = self.subject.center();
        let radius = self.subject.bounding_radius().max(1e-3);
        let distance = radius * self.distance_factor;

        // Inverse-square: doubling the distance needs four times the power.
        // Scaling with the subject's size keeps a teapot and a building both
        // exposed roughly correctly.
        let key_energy = self.key_energy.unwrap_or(40.0 * distance * distance);

        let front = self
            .facing
            .normalized()
            .unwrap_or_else(|| Vec3::new(0.0, -1.0, 0.0));
        // The horizontal axis perpendicular to the facing direction.
        let right = Vec3::new(-front.y, front.x, 0.0)
            .normalized()
            .unwrap_or(Vec3::new(1.0, 0.0, 0.0));
        let up = Vec3::new(0.0, 0.0, 1.0);

        // Key: 45 degrees off the facing axis and about 30 degrees up.
        let key_direction = (front * 0.7 + right * 0.7 + up * 0.55)
            .normalized()
            .unwrap_or(front);
        // Fill: opposite side, lower and flatter.
        let fill_direction = (front * 0.85 - right * 0.6 + up * 0.15)
            .normalized()
            .unwrap_or(front);
        // Rim: behind the subject, high, opposite the key.
        let rim_direction = (front * -0.9 - right * 0.35 + up * 0.7)
            .normalized()
            .unwrap_or(front);

        let key_size = radius * 1.6;
        let fill_size = radius * 2.4;

        let lights = vec![
            PlannedLight {
                role: "Key".into(),
                name: "Key".into(),
                light_type: LightType::Area,
                location: target + key_direction * distance,
                look_at: target,
                settings: LightSettings {
                    energy: Some(key_energy),
                    temperature: Some(self.key_temperature),
                    shape: Some(AreaShape::Square),
                    size: Some(key_size),
                    ..Default::default()
                },
            },
            PlannedLight {
                role: "Fill".into(),
                name: "Fill".into(),
                light_type: LightType::Area,
                location: target + fill_direction * (distance * 1.1),
                look_at: target,
                settings: LightSettings {
                    energy: Some(key_energy * self.fill_ratio),
                    temperature: Some(self.fill_temperature),
                    shape: Some(AreaShape::Square),
                    size: Some(fill_size),
                    // A fill that adds highlights stops being a fill: it is
                    // there to lift the shadows, not to add a second specular.
                    specular_factor: Some(0.15),
                    ..Default::default()
                },
            },
            PlannedLight {
                role: "Rim".into(),
                name: "Rim".into(),
                light_type: LightType::Area,
                location: target + rim_direction * (distance * 0.9),
                look_at: target,
                settings: LightSettings {
                    energy: Some(key_energy * self.rim_ratio),
                    temperature: Some(self.rim_temperature),
                    shape: Some(AreaShape::Rectangle),
                    size: Some(radius * 0.4),
                    size_y: Some(radius * 2.0),
                    ..Default::default()
                },
            },
        ];

        Ok(ThreePointPlan {
            lights,
            target,
            radius,
            distance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ThreePointSpec {
        ThreePointSpec {
            subject: Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0)),
            key_energy: Some(1000.0),
            fill_ratio: default_fill_ratio(),
            rim_ratio: default_rim_ratio(),
            distance_factor: default_distance_factor(),
            key_temperature: default_key_temperature(),
            fill_temperature: default_fill_temperature(),
            rim_temperature: default_rim_temperature(),
            facing: default_facing(),
        }
    }

    #[test]
    fn the_rig_has_three_lights_all_aimed_at_the_subject() {
        let plan = spec().plan().unwrap();
        assert_eq!(plan.lights.len(), 3);
        for light in &plan.lights {
            assert_eq!(light.look_at, plan.target);
        }
        let roles: Vec<&str> = plan.lights.iter().map(|l| l.role.as_str()).collect();
        assert_eq!(roles, ["Key", "Fill", "Rim"]);
    }

    #[test]
    fn the_key_is_the_brightest_and_the_fill_the_dimmest() {
        let plan = spec().plan().unwrap();
        let energy = |role: &str| {
            plan.lights
                .iter()
                .find(|l| l.role == role)
                .unwrap()
                .settings
                .energy
                .unwrap()
        };
        assert!(energy("Key") > energy("Rim"));
        assert!(energy("Rim") > energy("Fill"));
        assert!((energy("Fill") / energy("Key") - 0.35).abs() < 1e-9);
    }

    #[test]
    fn key_and_fill_are_on_opposite_sides() {
        let plan = spec().plan().unwrap();
        let key = plan.lights[0].location - plan.target;
        let fill = plan.lights[1].location - plan.target;
        // Their sideways components must have opposite sign, which is what
        // makes it a key and a fill rather than two keys.
        assert!(
            key.x * fill.x < 0.0,
            "key {key:?} and fill {fill:?} are on the same side"
        );
    }

    #[test]
    fn the_rim_is_behind_the_subject() {
        let plan = spec().plan().unwrap();
        let front = default_facing().normalized().unwrap();
        let rim = (plan.lights[2].location - plan.target)
            .normalized()
            .unwrap();
        assert!(
            rim.dot(front) < 0.0,
            "the rim should be opposite the facing direction"
        );
    }

    #[test]
    fn the_fill_does_not_add_highlights() {
        let plan = spec().plan().unwrap();
        let fill = &plan.lights[1];
        assert!(fill.settings.specular_factor.unwrap() < 0.5);
    }

    #[test]
    fn energy_scales_with_the_square_of_distance_when_not_given() {
        let mut small = spec();
        small.key_energy = None;
        let mut large = small.clone();
        large.subject = Aabb::new(Vec3::splat(-2.0), Vec3::splat(2.0));

        let small_energy = small.plan().unwrap().lights[0].settings.energy.unwrap();
        let large_energy = large.plan().unwrap().lights[0].settings.energy.unwrap();
        // Twice the radius is twice the distance, so four times the power.
        assert!((large_energy / small_energy - 4.0).abs() < 1e-6);
    }

    #[test]
    fn the_key_is_warmer_than_the_fill() {
        let plan = spec().plan().unwrap();
        let key_temperature = plan.lights[0].settings.temperature.unwrap();
        let fill_temperature = plan.lights[1].settings.temperature.unwrap();
        assert!(key_temperature < fill_temperature, "lower Kelvin is warmer");
    }

    #[test]
    fn absurd_ratios_are_refused() {
        let mut bad = spec();
        bad.fill_ratio = 10.0;
        assert!(bad.plan().is_err());
    }

    #[test]
    fn the_rig_follows_the_facing_direction() {
        let mut turned = spec();
        turned.facing = Vec3::new(1.0, 0.0, 0.0);
        let plan = turned.plan().unwrap();
        let key = (plan.lights[0].location - plan.target)
            .normalized()
            .unwrap();
        assert!(
            key.x > 0.0,
            "the key should move round with the subject: {key:?}"
        );
    }
}

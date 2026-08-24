//! Light payloads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{CollectionRef, LightId, ObjectRef},
    math::{Color4, Finite, Vec3, check_non_negative, check_positive, check_range},
};

/// Blender light types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LightType {
    Point,
    Sun,
    Spot,
    Area,
}

/// Area light shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AreaShape {
    Square,
    Rectangle,
    Disk,
    Ellipse,
}

/// Light properties. Which fields apply depends on the light type; fields that
/// do not apply are ignored rather than rejected, so a caller can retarget a
/// light template across types.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LightSettings {
    /// Radiant power. Watts for point/spot/area, irradiance for sun.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color4>,
    /// Colour temperature in Kelvin. Converted to a colour server-side, so it
    /// works identically on every Blender build. Overrides `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Shadow softness radius for point and spot lights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// Angular diameter of the sun disc, in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<AreaShape>,
    /// Area light size along X (or the single dimension for square/disk).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Area light size along Y, for rectangle and ellipse shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_y: Option<f64>,
    /// Spot cone angle in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot_size: Option<f64>,
    /// Spot edge softness, 0..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot_blend: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_shadow: Option<bool>,
    /// Multiplier on diffuse contribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffuse_factor: Option<f64>,
    /// Multiplier on specular contribution. Dropping this is the standard trick
    /// for a fill light that should not add highlights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specular_factor: Option<f64>,
    /// Multiplier on volumetric contribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_factor: Option<f64>,
    /// Cycles-only: how many bounces the light contributes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bounces: Option<u32>,
}

impl Validate for LightSettings {
    fn validate(&self) -> Result<()> {
        if let Some(energy) = self.energy {
            check_non_negative(energy, "energy")?;
        }
        if let Some(color) = self.color {
            color.check_finite("color")?;
        }
        if let Some(temperature) = self.temperature {
            check_range(temperature, 1000.0, 40000.0, "temperature")?;
        }
        for (value, field) in [
            (self.radius, "radius"),
            (self.size, "size"),
            (self.size_y, "size_y"),
        ] {
            if let Some(v) = value {
                check_non_negative(v, field)?;
            }
        }
        if let Some(angle) = self.angle {
            check_range(angle, 0.0, std::f64::consts::PI, "angle")?;
        }
        if let Some(spot_size) = self.spot_size {
            check_range(spot_size, 0.0, std::f64::consts::PI, "spot_size")?;
        }
        if let Some(spot_blend) = self.spot_blend {
            check_range(spot_blend, 0.0, 1.0, "spot_blend")?;
        }
        for (value, field) in [
            (self.diffuse_factor, "diffuse_factor"),
            (self.specular_factor, "specular_factor"),
            (self.volume_factor, "volume_factor"),
        ] {
            if let Some(v) = value {
                check_non_negative(v, field)?;
            }
        }
        Ok(())
    }
}

/// `light.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateLight {
    #[serde(rename = "type")]
    pub light_type: LightType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    /// Aim the light at this point instead of specifying a rotation. The
    /// rotation is computed server-side from the location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_at: Option<Vec3>,
    /// Aim the light at an object's bounding-box centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    #[serde(default, flatten)]
    pub settings: LightSettings,
}

impl Validate for CreateLight {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.location.check_finite("location")?;
        self.look_at.check_finite("look_at")?;
        self.rotation.check_finite("rotation")?;
        let aims = [
            self.look_at.is_some(),
            self.target.is_some(),
            self.rotation.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        if aims > 1 {
            return Err(BlenderError::invalid_argument(
                "Set at most one of `look_at`, `target` or `rotation`.",
            ));
        }
        if matches!(self.light_type, LightType::Point) && self.spot_or_area_only() {
            // Not fatal: extra fields are ignored. Nothing to reject.
        }
        self.settings.validate()
    }
}

impl CreateLight {
    fn spot_or_area_only(&self) -> bool {
        self.settings.spot_size.is_some()
            || self.settings.spot_blend.is_some()
            || self.settings.shape.is_some()
    }
}

/// `light.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateLight {
    pub light: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Change the light type in place, keeping its transform and name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_type: Option<LightType>,
    #[serde(default, flatten)]
    pub settings: LightSettings,
}

impl Validate for UpdateLight {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.settings.validate()
    }
}

/// `light.look_at`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LookAt {
    pub light: ObjectRef,
    /// Explicit world-space aim point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<Vec3>,
    /// Aim at an object's bounding-box centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
    /// Also move the light to this distance from the target, along the current
    /// aim direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
}

impl Validate for LookAt {
    fn validate(&self) -> Result<()> {
        match (self.point, &self.target) {
            (None, None) => Err(BlenderError::invalid_argument(
                "Provide `point` or `target`.",
            )),
            (Some(_), Some(_)) => Err(BlenderError::invalid_argument(
                "Provide `point` or `target`, not both.",
            )),
            _ => {
                self.point.check_finite("point")?;
                if let Some(distance) = self.distance {
                    check_positive(distance, "distance")?;
                }
                Ok(())
            }
        }
    }
}

/// `light.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListLights {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_type: Option<LightType>,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListLights {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// A light as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LightSummary {
    /// Id of the light *object*.
    pub id: crate::ids::ObjectId,
    /// Id of the light data-block.
    pub data_id: LightId,
    pub name: String,
    #[serde(rename = "type")]
    pub light_type: LightType,
    pub location: Vec3,
    pub rotation_euler: Vec3,
    #[serde(flatten)]
    pub settings: LightSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_blend_is_normalised() {
        let settings = LightSettings {
            spot_blend: Some(1.5),
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn negative_energy_is_rejected() {
        let settings = LightSettings {
            energy: Some(-10.0),
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn only_one_aiming_method_is_allowed() {
        let params = CreateLight {
            light_type: LightType::Area,
            name: None,
            location: Some(Vec3::new(0.0, 0.0, 3.0)),
            look_at: Some(Vec3::ZERO),
            target: Some(ObjectRef::name("Cube")),
            rotation: None,
            collection: None,
            settings: LightSettings::default(),
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn look_at_needs_exactly_one_target() {
        let neither = LookAt {
            light: ObjectRef::name("Key"),
            point: None,
            target: None,
            distance: None,
        };
        assert!(neither.validate().is_err());
        let one = LookAt {
            light: ObjectRef::name("Key"),
            point: Some(Vec3::ZERO),
            target: None,
            distance: Some(4.0),
        };
        assert!(one.validate().is_ok());
    }
}

/// `light.get` / `light.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LightRefParams {
    pub light: ObjectRef,
}

impl Validate for LightRefParams {}

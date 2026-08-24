//! Material payloads.
//!
//! The Principled BSDF is the practical centre of Blender shading, so it gets a
//! first-class typed surface. Its socket names moved in 4.0 (`Specular` ->
//! `Specular IOR Level`, `Emission` -> `Emission Color`, subsurface reworked),
//! which is exactly why callers set *semantic* fields here and the bridge maps
//! them to whatever the connected build actually exposes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{MaterialId, MaterialRef, ObjectRef},
    math::{Color4, Finite, check_non_negative, check_range},
};

/// Semantic Principled BSDF inputs. Every field is optional; omitted fields are
/// left untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PrincipledInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color: Option<Color4>,
    /// 0 = dielectric, 1 = metal. Values between are physically meaningless
    /// except as a blend for mixed surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f64>,
    /// Index of refraction. 1.45 for glass, 1.33 for water, 1.5 default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ior: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_color: Option<Color4>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_strength: Option<f64>,
    /// Specular reflection level for dielectrics (`Specular IOR Level` in 4.x).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specular: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transmission: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coat_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coat_roughness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheen_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheen_roughness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anisotropic: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anisotropic_rotation: Option<f64>,
    /// Strength multiplier for an attached normal map, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_strength: Option<f64>,
}

impl PrincipledInputs {
    /// `true` when nothing was requested, so callers can skip a round trip.
    pub fn is_empty(&self) -> bool {
        serde_json::to_value(self)
            .map(|v| v.as_object().is_none_or(|o| o.is_empty()))
            .unwrap_or(false)
    }
}

impl Validate for PrincipledInputs {
    fn validate(&self) -> Result<()> {
        for (value, field) in [
            (self.metallic, "metallic"),
            (self.roughness, "roughness"),
            (self.alpha, "alpha"),
            (self.transmission, "transmission"),
            (self.coat_weight, "coat_weight"),
            (self.coat_roughness, "coat_roughness"),
            (self.sheen_weight, "sheen_weight"),
            (self.sheen_roughness, "sheen_roughness"),
            (self.anisotropic, "anisotropic"),
        ] {
            if let Some(v) = value {
                check_range(v, 0.0, 1.0, field)?;
            }
        }
        if let Some(ior) = self.ior {
            // Below 1.0 is not a refractive index; above 3.0 is beyond any
            // material Cycles models usefully.
            check_range(ior, 1.0, 3.0, "ior")?;
        }
        if let Some(specular) = self.specular {
            check_range(specular, 0.0, 2.0, "specular")?;
        }
        for (value, field) in [
            (self.emission_strength, "emission_strength"),
            (self.normal_strength, "normal_strength"),
        ] {
            if let Some(v) = value {
                check_non_negative(v, field)?;
            }
        }
        if let Some(color) = self.base_color {
            color.check_finite("base_color")?;
        }
        if let Some(color) = self.emission_color {
            color.check_finite("emission_color")?;
        }
        if let Some(rotation) = self.anisotropic_rotation {
            check_range(rotation, 0.0, 1.0, "anisotropic_rotation")?;
        }
        Ok(())
    }
}

/// How a material handles transparency in EEVEE and in the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlendMode {
    Opaque,
    Clip,
    Hashed,
    Blend,
}

/// Which side of a face is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DisplacementMethod {
    Bump,
    Displacement,
    BothDisplacementAndBump,
}

/// Non-shader material settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MaterialSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_method: Option<BlendMode>,
    /// Render both faces of single-sided geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_backface_culling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displacement_method: Option<DisplacementMethod>,
    /// Viewport solid-mode colour. Purely cosmetic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_color: Option<Color4>,
    /// Alpha threshold for `CLIP` blending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_threshold: Option<f64>,
}

impl Validate for MaterialSettings {
    fn validate(&self) -> Result<()> {
        if let Some(color) = self.viewport_color {
            color.check_finite("viewport_color")?;
        }
        if let Some(threshold) = self.alpha_threshold {
            check_range(threshold, 0.0, 1.0, "alpha_threshold")?;
        }
        Ok(())
    }
}

/// `material.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateMaterial {
    pub name: String,
    /// Start from a Principled BSDF wired to the output. Turn this off to get
    /// an empty node tree to build by hand.
    #[serde(default = "crate::object::default_true")]
    pub use_nodes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principled: Option<PrincipledInputs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<MaterialSettings>,
    /// Objects to assign the new material to immediately.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assign_to: Vec<ObjectRef>,
}

impl Validate for CreateMaterial {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")?;
        if let Some(principled) = &self.principled {
            if !self.use_nodes {
                return Err(BlenderError::invalid_argument(
                    "`principled` requires `use_nodes: true`; a material without nodes has no Principled BSDF.",
                ));
            }
            principled.validate()?;
        }
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        Ok(())
    }
}

/// `material.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateMaterial {
    pub material: MaterialRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principled: Option<PrincipledInputs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<MaterialSettings>,
}

impl Validate for UpdateMaterial {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if let Some(principled) = &self.principled {
            principled.validate()?;
        }
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        if self.name.is_none() && self.principled.is_none() && self.settings.is_none() {
            return Err(BlenderError::invalid_argument(
                "`material.update` needs at least one of name, principled or settings.",
            ));
        }
        Ok(())
    }
}

/// `material.assign`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssignMaterial {
    pub material: MaterialRef,
    pub objects: Vec<ObjectRef>,
    /// Slot to write into. Omit to append a new slot, or reuse slot 0 when the
    /// object has exactly one empty slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_index: Option<u32>,
    /// Replace every existing slot instead of writing one.
    #[serde(default)]
    pub replace_all: bool,
    /// Restrict the assignment to these face indices. Requires exactly one
    /// object, and is subject to the mesh revision check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub face_indices: Vec<u32>,
    /// Mesh revision the `face_indices` were read at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
}

impl Validate for AssignMaterial {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        if !self.face_indices.is_empty() && self.objects.len() != 1 {
            return Err(BlenderError::invalid_argument(
                "`face_indices` addresses one mesh, so exactly one object may be given.",
            )
            .with_detail("objects", self.objects.len()));
        }
        if self.replace_all && self.slot_index.is_some() {
            return Err(BlenderError::invalid_argument(
                "`replace_all` and `slot_index` contradict each other.",
            ));
        }
        Ok(())
    }
}

/// `material.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListMaterials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only materials used by this object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_by: Option<ObjectRef>,
    /// Only materials with no users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused: Option<bool>,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListMaterials {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// A material as reported by `material.get` / `material.list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MaterialSummary {
    pub id: MaterialId,
    pub name: String,
    #[serde(default)]
    pub use_nodes: bool,
    #[serde(default)]
    pub users: u32,
    /// Present when the material has a Principled BSDF the bridge could read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principled: Option<PrincipledInputs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<MaterialSettings>,
    #[serde(default)]
    pub node_count: u32,
    /// Image texture files this material references, for missing-texture hunts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_are_enforced_on_normalised_inputs() {
        let inputs = PrincipledInputs {
            roughness: Some(1.5),
            ..Default::default()
        };
        let err = inputs.validate().unwrap_err();
        assert!(err.message.contains("roughness"), "{}", err.message);
    }

    #[test]
    fn ior_below_one_is_rejected() {
        let inputs = PrincipledInputs {
            ior: Some(0.5),
            ..Default::default()
        };
        assert!(inputs.validate().is_err());
    }

    #[test]
    fn emission_strength_may_exceed_one() {
        let inputs = PrincipledInputs {
            emission_strength: Some(50.0),
            ..Default::default()
        };
        assert!(inputs.validate().is_ok());
    }

    #[test]
    fn principled_without_nodes_is_rejected() {
        let params = CreateMaterial {
            name: "Concrete".into(),
            use_nodes: false,
            principled: Some(PrincipledInputs {
                roughness: Some(0.5),
                ..Default::default()
            }),
            settings: None,
            assign_to: vec![],
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn face_assignment_requires_a_single_object() {
        let params = AssignMaterial {
            material: MaterialRef::name("Concrete"),
            objects: vec![ObjectRef::name("A"), ObjectRef::name("B")],
            slot_index: None,
            replace_all: false,
            face_indices: vec![1, 2],
            expected_mesh_revision: None,
        };
        assert!(params.validate().is_err());
    }
}

/// `material.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetMaterial {
    pub material: MaterialRef,
}

/// `material.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteMaterial {
    pub material: MaterialRef,
    /// Delete even though other data-blocks still use it.
    #[serde(default)]
    pub force: bool,
}

/// `material.duplicate`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DuplicateMaterial {
    pub material: MaterialRef,
    /// Name for the copy. Blender appends a numeric suffix if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `material.unassign`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnassignMaterial {
    pub objects: Vec<ObjectRef>,
    /// Only clear slots holding this material. Omit to clear every slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialRef>,
    /// Remove the emptied slot rather than leaving it blank.
    #[serde(default = "crate::object::default_true")]
    pub remove_slot: bool,
}

/// `material.slot.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListMaterialSlots {
    pub object: ObjectRef,
}

impl Validate for GetMaterial {}
impl Validate for DeleteMaterial {}
impl Validate for ListMaterialSlots {}

impl Validate for DuplicateMaterial {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        Ok(())
    }
}

impl Validate for UnassignMaterial {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        Ok(())
    }
}

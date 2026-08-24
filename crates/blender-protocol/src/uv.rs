//! UV, image and texture-baking payloads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, ErrorCode, Page, Result, Validate, check_name,
    ids::{ImageId, ImageRef, ObjectRef},
    io::ManagedPath,
    math::{check_positive, check_range},
    mesh::ElementSelection,
};

/// Unwrap algorithms and projections the bridge exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UnwrapMethod {
    /// Angle-based flattening. Blender's default; good general-purpose result.
    AngleBased,
    /// Least-squares conformal maps. Better for organic shapes with good seams.
    Conformal,
    /// Minimum stretch, available in newer Blender builds.
    MinimumStretch,
    /// Automatic seams by face angle. No seams required.
    SmartProject,
    CubeProject,
    CylinderProject,
    SphereProject,
    /// Project from the current viewport view.
    ProjectFromView,
}

impl UnwrapMethod {
    /// Whether the method needs seams to produce a usable layout.
    pub const fn needs_seams(self) -> bool {
        matches!(
            self,
            UnwrapMethod::AngleBased | UnwrapMethod::Conformal | UnwrapMethod::MinimumStretch
        )
    }

    /// Whether the method depends on the viewport, and so cannot run headless.
    pub const fn needs_viewport(self) -> bool {
        matches!(self, UnwrapMethod::ProjectFromView)
    }
}

/// `uv.unwrap.*` and the projection operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Unwrap {
    pub object: ObjectRef,
    /// Which projection to use.
    ///
    /// Every tool that takes this sets it itself from its own name -- there is
    /// a tool per method precisely so a caller does not have to choose one --
    /// so it is defaulted and hidden from the schema. Leaving it required made
    /// all seven projection tools reject every call for a missing field the
    /// handler was about to overwrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub method: Option<UnwrapMethod>,
    /// UV map to write into. Created if missing; defaults to the active map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_map: Option<String>,
    /// Restrict to these faces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ElementSelection>,
    /// Margin between islands, 0..1.
    #[serde(default = "default_margin")]
    pub margin: f64,
    /// Smart-project angle limit, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_limit: Option<f64>,
    /// Keep island proportions consistent with world-space area.
    #[serde(default)]
    pub correct_aspect: bool,
    /// Scale islands to fit the 0-1 UV square.
    #[serde(default = "crate::object::default_true")]
    pub scale_to_bounds: bool,
    /// Cube/cylinder/sphere projection size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_size: Option<f64>,
}

fn default_margin() -> f64 {
    0.001
}

impl Validate for Unwrap {
    fn validate(&self) -> Result<()> {
        if let Some(uv_map) = &self.uv_map {
            check_name(uv_map, "uv_map")?;
        }
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        check_range(self.margin, 0.0, 1.0, "margin")?;
        if let Some(angle) = self.angle_limit {
            check_range(angle, 0.0, 89.0, "angle_limit")?;
            // Validation runs before the tool stamps its own method in, so
            // this can only judge a method the caller supplied explicitly.
            // The bridge reads `angle_limit` for smart-project alone, so a
            // stray one elsewhere is ignored rather than acted on.
            if let Some(method) = self.method
                && method != UnwrapMethod::SmartProject
            {
                return Err(BlenderError::invalid_argument(
                    "`angle_limit` only applies to `SMART_PROJECT`.",
                )
                .with_detail("method", format!("{method:?}")));
            }
        }
        if let Some(size) = self.projection_size {
            check_positive(size, "projection_size")?;
        }
        Ok(())
    }
}

/// `uv.pack_islands`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PackIslands {
    pub objects: Vec<ObjectRef>,
    #[serde(default = "default_margin")]
    pub margin: f64,
    /// Rotate islands to pack more tightly.
    #[serde(default = "crate::object::default_true")]
    pub rotate: bool,
    /// Pack every object into one shared UV space, for atlasing.
    #[serde(default)]
    pub pack_together: bool,
    /// Keep the aspect ratio of the target image in mind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udim_source: Option<String>,
}

impl Validate for PackIslands {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        check_range(self.margin, 0.0, 1.0, "margin")?;
        if self.pack_together && self.objects.len() < 2 {
            return Err(BlenderError::invalid_argument(
                "`pack_together` needs at least two objects to be meaningful.",
            ));
        }
        Ok(())
    }
}

/// `uv.mark_seam` / `uv.clear_seam`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkSeam {
    pub object: ObjectRef,
    /// Edges to mark or clear.
    pub selection: ElementSelection,
    /// Clear instead of mark.
    #[serde(default)]
    pub clear: bool,
}

impl Validate for MarkSeam {
    fn validate(&self) -> Result<()> {
        self.selection.validate()?;
        if self.selection.element_type != crate::mesh::ElementType::Edge {
            return Err(BlenderError::invalid_argument("Seams are marked on edges."));
        }
        if self.selection.is_everything() {
            return Err(BlenderError::invalid_argument(
                "Marking every edge as a seam is almost never intended; pass explicit edge indices.",
            ));
        }
        Ok(())
    }
}

/// `uv.map.create` / `delete` / `set_active`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UvMapOperation {
    pub object: ObjectRef,
    pub name: String,
    /// Copy the active map's coordinates into the new one.
    #[serde(default)]
    pub copy_active: bool,
}

impl Validate for UvMapOperation {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")
    }
}

/// `image.repath`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepathImages {
    /// Folder holding the texture files, relative to a managed root.
    #[serde(flatten)]
    pub directory: ManagedPath,
    /// Which images to repoint. Omit for every file-backed image in the scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Only touch images whose file is missing. On by default, so a working
    /// texture is never quietly pointed somewhere else.
    #[serde(default = "crate::uv::yes")]
    pub only_missing: bool,
    /// Pack the pixels into the .blend once found, so the link cannot break
    /// again.
    #[serde(default)]
    pub pack: bool,
}

impl Validate for RepathImages {
    fn validate(&self) -> Result<()> {
        self.directory.validate()
    }
}

/// `image.load`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoadImage {
    #[serde(flatten)]
    pub source: ManagedPath,
    /// Data-block name. Defaults to the filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Store the pixels inside the .blend rather than referencing the file.
    #[serde(default)]
    pub pack: bool,
    /// Colour space: `sRGB` for colour maps, `Non-Color` for data maps such as
    /// normal, roughness and metallic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    /// Whether the image is a UDIM tile set.
    #[serde(default)]
    pub tiled: bool,
}

impl Validate for LoadImage {
    fn validate(&self) -> Result<()> {
        self.source.validate()?;
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        const IMAGE_EXTENSIONS: [&str; 11] = [
            "png", "jpg", "jpeg", "exr", "hdr", "tif", "tiff", "tga", "webp", "bmp", "dds",
        ];
        match self.source.extension() {
            Some(extension) if IMAGE_EXTENSIONS.contains(&extension.as_str()) => Ok(()),
            Some(extension) => Err(BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!("`.{extension}` is not an image format Blender loads."),
            )
            .with_detail("extension", extension)
            .with_detail_json("supported", &IMAGE_EXTENSIONS.to_vec())),
            None => Err(BlenderError::new(
                ErrorCode::InvalidPath,
                "The image path has no extension, so its format cannot be determined.",
            )),
        }
    }
}

/// Bake pass types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BakeType {
    Combined,
    AmbientOcclusion,
    Normal,
    Diffuse,
    Glossy,
    Roughness,
    Emit,
    Shadow,
    Position,
    Uv,
    Environment,
    Transmission,
}

impl BakeType {
    /// Blender's identifier for this pass.
    pub const fn blender_id(self) -> &'static str {
        match self {
            BakeType::Combined => "COMBINED",
            BakeType::AmbientOcclusion => "AO",
            BakeType::Normal => "NORMAL",
            BakeType::Diffuse => "DIFFUSE",
            BakeType::Glossy => "GLOSSY",
            BakeType::Roughness => "ROUGHNESS",
            BakeType::Emit => "EMIT",
            BakeType::Shadow => "SHADOW",
            BakeType::Position => "POSITION",
            BakeType::Uv => "UV",
            BakeType::Environment => "ENVIRONMENT",
            BakeType::Transmission => "TRANSMISSION",
        }
    }

    /// Whether the pass should be written as non-colour data.
    pub const fn is_data(self) -> bool {
        matches!(
            self,
            BakeType::Normal
                | BakeType::Roughness
                | BakeType::Position
                | BakeType::Uv
                | BakeType::AmbientOcclusion
        )
    }
}

/// `texture.bake`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Bake {
    /// Object receiving the baked texture.
    pub target: ObjectRef,
    /// High-poly sources, for a selected-to-active bake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ObjectRef>,
    #[serde(rename = "type")]
    pub bake_type: BakeType,
    #[serde(default = "default_bake_resolution")]
    pub width: u32,
    #[serde(default = "default_bake_resolution")]
    pub height: u32,
    /// Pixels of bleed outside each island. Prevents seams at low mip levels.
    #[serde(default = "default_bake_margin")]
    pub margin: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    /// UV map to bake into. Defaults to the active one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_map: Option<String>,
    /// Ray distance for a selected-to-active bake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cage_extrusion: Option<f64>,
    /// Explicit cage object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cage_object: Option<ObjectRef>,
    /// Maximum ray distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ray_distance: Option<f64>,
    /// Output format for the saved image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<crate::render::ImageFormat>,
    /// Base filename for the produced artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Also wire the baked image into the target's material.
    #[serde(default)]
    pub connect_to_material: bool,
}

fn default_bake_resolution() -> u32 {
    1024
}
fn default_bake_margin() -> u32 {
    16
}

impl Validate for Bake {
    fn validate(&self) -> Result<()> {
        for (value, field) in [(self.width, "width"), (self.height, "height")] {
            if value == 0 || value > 16384 {
                return Err(BlenderError::invalid_argument(format!(
                    "`{field}` must be between 1 and 16384, got {value}."
                )));
            }
            if !value.is_power_of_two() {
                // Not fatal -- Blender bakes any size -- but engines mip
                // non-power-of-two textures badly, so it is worth surfacing.
            }
        }
        if self.margin > 64 {
            return Err(BlenderError::invalid_argument(
                "`margin` above 64 pixels wastes most of the texture.",
            ));
        }
        if let Some(samples) = self.samples
            && (samples == 0 || samples > 10_000)
        {
            return Err(BlenderError::invalid_argument(
                "`samples` must be between 1 and 10000.",
            ));
        }
        if let Some(extrusion) = self.cage_extrusion {
            crate::math::check_non_negative(extrusion, "cage_extrusion")?;
        }
        if let Some(distance) = self.max_ray_distance {
            crate::math::check_non_negative(distance, "max_ray_distance")?;
        }
        if let Some(uv_map) = &self.uv_map {
            check_name(uv_map, "uv_map")?;
        }
        if let Some(name) = &self.name {
            crate::render::check_artifact_name(name)?;
        }
        if (self.cage_object.is_some() || self.cage_extrusion.is_some()) && self.sources.is_empty()
        {
            return Err(BlenderError::invalid_argument(
                "Cage settings only apply to a selected-to-active bake, which needs `sources`.",
            ));
        }
        if self.sources.contains(&self.target) {
            return Err(BlenderError::invalid_argument(
                "The bake target cannot also be a source.",
            ));
        }
        Ok(())
    }
}

/// An image as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageSummary {
    pub id: ImageId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filepath: Option<String>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub channels: u32,
    #[serde(default)]
    pub is_packed: bool,
    /// True when the file the image points at is missing on disk.
    #[serde(default)]
    pub is_missing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    #[serde(default)]
    pub users: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// `image.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListImages {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only images whose file is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<bool>,
    /// Only images with no users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused: Option<bool>,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListImages {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// `image.remove` / `image.reload`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageOperation {
    pub image: ImageRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bake() -> Bake {
        Bake {
            target: ObjectRef::name("LowPoly"),
            sources: vec![],
            bake_type: BakeType::Normal,
            width: 1024,
            height: 1024,
            margin: 16,
            samples: None,
            uv_map: None,
            cage_extrusion: None,
            cage_object: None,
            max_ray_distance: None,
            format: None,
            name: None,
            connect_to_material: false,
        }
    }

    #[test]
    fn cage_settings_require_sources() {
        let params = Bake {
            cage_extrusion: Some(0.1),
            ..bake()
        };
        assert!(params.validate().is_err());

        let params = Bake {
            cage_extrusion: Some(0.1),
            sources: vec![ObjectRef::name("HighPoly")],
            ..bake()
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn a_bake_target_cannot_be_its_own_source() {
        let params = Bake {
            sources: vec![ObjectRef::name("LowPoly")],
            ..bake()
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn angle_limit_only_applies_to_smart_project() {
        let params = Unwrap {
            object: ObjectRef::name("Cube"),
            method: Some(UnwrapMethod::AngleBased),
            uv_map: None,
            selection: None,
            margin: 0.001,
            angle_limit: Some(66.0),
            correct_aspect: false,
            scale_to_bounds: true,
            projection_size: None,
        };
        assert!(params.validate().is_err());

        let params = Unwrap {
            method: Some(UnwrapMethod::SmartProject),
            ..params
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn non_image_files_are_refused() {
        let params = LoadImage {
            source: crate::io::ManagedPath::project("model.fbx"),
            name: None,
            pack: false,
            colorspace: None,
            tiled: false,
        };
        assert_eq!(
            params.validate().unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );
    }

    #[test]
    fn data_passes_are_flagged_as_non_colour() {
        assert!(BakeType::Normal.is_data());
        assert!(!BakeType::Combined.is_data());
        assert_eq!(BakeType::AmbientOcclusion.blender_id(), "AO");
    }

    #[test]
    fn unwrap_methods_declare_their_needs() {
        assert!(UnwrapMethod::AngleBased.needs_seams());
        assert!(!UnwrapMethod::SmartProject.needs_seams());
        assert!(UnwrapMethod::ProjectFromView.needs_viewport());
    }
}

/// `uv.maps.list` / `uv.average_island_scale`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UvObjectParams {
    pub object: ObjectRef,
    /// UV map to act on. Defaults to the active one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_map: Option<String>,
}

/// `image.get` / `image.reload` / `image.remove`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageRefParams {
    pub image: ImageRef,
}

impl Validate for ImageRefParams {}
impl Validate for ImageOperation {}

impl Validate for UvObjectParams {
    fn validate(&self) -> Result<()> {
        if let Some(uv_map) = &self.uv_map {
            check_name(uv_map, "uv_map")?;
        }
        Ok(())
    }
}

/// Serde default for flags that are on unless asked otherwise.
pub(crate) fn yes() -> bool {
    true
}

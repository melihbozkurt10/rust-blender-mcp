//! Render payloads.
//!
//! Output never goes to a caller-supplied absolute path. Renders land in a
//! managed directory the server owns and come back as an artifact reference, so
//! "render to `C:\Windows\System32\...`" is not expressible.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Result, Validate, check_frame_range,
    ids::{ArtifactId, ObjectRef},
    math::{check_positive, check_range},
};

/// Render engines the bridge exposes. Availability is still checked against the
/// connected build -- EEVEE's identifier changed in 4.2 and Workbench is
/// missing from some builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RenderEngine {
    Cycles,
    /// Whichever EEVEE this build ships. The bridge maps this to
    /// `BLENDER_EEVEE_NEXT` or `BLENDER_EEVEE` as appropriate.
    Eevee,
    Workbench,
}

impl RenderEngine {
    /// Candidate Blender identifiers, most preferred first.
    pub const fn candidates(self) -> &'static [&'static str] {
        match self {
            RenderEngine::Cycles => &["CYCLES"],
            RenderEngine::Eevee => &["BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"],
            RenderEngine::Workbench => &["BLENDER_WORKBENCH"],
        }
    }
}

/// Image output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
    OpenExr,
    OpenExrMultilayer,
    Tiff,
    Webp,
    Targa,
}

impl ImageFormat {
    pub const fn blender_id(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::OpenExr => "OPEN_EXR",
            ImageFormat::OpenExrMultilayer => "OPEN_EXR_MULTILAYER",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Webp => "WEBP",
            ImageFormat::Targa => "TARGA",
        }
    }

    pub const fn mime_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::OpenExr | ImageFormat::OpenExrMultilayer => "image/x-exr",
            ImageFormat::Tiff => "image/tiff",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Targa => "image/x-tga",
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::OpenExr | ImageFormat::OpenExrMultilayer => "exr",
            ImageFormat::Tiff => "tif",
            ImageFormat::Webp => "webp",
            ImageFormat::Targa => "tga",
        }
    }

    /// Whether this format can carry an alpha channel.
    pub const fn supports_alpha(self) -> bool {
        !matches!(self, ImageFormat::Jpeg)
    }
}

/// Colour channel layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ColorMode {
    Bw,
    Rgb,
    Rgba,
}

/// Render settings. Every field is optional and omitted fields are untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RenderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<RenderEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_x: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_y: Option<u32>,
    /// Percentage of the resolution actually rendered, 1-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_percentage: Option<u32>,
    /// Sample count. Maps to Cycles samples or EEVEE taa_render_samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    /// Render the world as transparent, for compositing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparent_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ImageFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_mode: Option<ColorMode>,
    /// Quality for lossy formats, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u32>,
    /// Cycles adaptive sampling threshold. 0 disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_threshold: Option<f64>,
    /// Cycles denoising.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise: Option<bool>,
    /// Maximum light bounces (Cycles).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bounces: Option<u32>,
    /// Motion blur.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<bool>,
    /// `Standard`, `Filmic`, `AgX`, `Khronos PBR Neutral`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_transform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub film_exposure: Option<f64>,
    /// Use the GPU where the build supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_gpu: Option<bool>,
}

impl Validate for RenderSettings {
    fn validate(&self) -> Result<()> {
        for (value, field) in [
            (self.resolution_x, "resolution_x"),
            (self.resolution_y, "resolution_y"),
        ] {
            if let Some(v) = value {
                if v == 0 {
                    return Err(BlenderError::invalid_argument(format!(
                        "`{field}` must be at least 1."
                    ))
                    .with_detail("field", field));
                }
                if v > 32_768 {
                    return Err(BlenderError::invalid_argument(format!(
                        "`{field}` of {v} exceeds the 32768 pixel limit."
                    ))
                    .with_detail("field", field));
                }
            }
        }
        if let Some(percentage) = self.resolution_percentage
            && !(1..=100).contains(&percentage)
        {
            return Err(BlenderError::invalid_argument(format!(
                "`resolution_percentage` must be between 1 and 100, got {percentage}."
            )));
        }
        if let Some(samples) = self.samples
            && (samples == 0 || samples > 100_000)
        {
            return Err(BlenderError::invalid_argument(format!(
                "`samples` must be between 1 and 100000, got {samples}."
            )));
        }
        if let Some(quality) = self.quality {
            check_range(quality as f64, 0.0, 100.0, "quality")?;
        }
        if let Some(threshold) = self.adaptive_threshold {
            check_range(threshold, 0.0, 1.0, "adaptive_threshold")?;
        }
        if let Some(exposure) = self.film_exposure {
            check_range(exposure, -32.0, 32.0, "film_exposure")?;
        }
        if let (Some(true), Some(format)) = (self.transparent_background, self.format)
            && !format.supports_alpha()
        {
            return Err(BlenderError::invalid_argument(format!(
                "`{}` cannot store alpha, so a transparent background would be lost.",
                format.blender_id()
            ))
            .with_detail("format", format.blender_id()));
        }
        Ok(())
    }
}

/// What to render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenderScope {
    /// A single frame.
    Frame(i32),
    /// An inclusive frame range, rendered as an image sequence.
    Range {
        start: i32,
        end: i32,
        #[serde(default = "one_step")]
        step: u32,
    },
    /// The scene's current frame.
    Current,
}

fn one_step() -> u32 {
    1
}

/// `render.execute`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteRender {
    /// Camera to render from. Omit to use the scene's active camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    #[serde(default = "default_scope")]
    pub scope: RenderScope,
    /// Base filename, without extension or directory. Sanitised server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, flatten)]
    pub settings: RenderSettings,
    /// Restore the scene's previous render settings afterwards. On by default:
    /// a render should not silently change the file the user is working in.
    #[serde(default = "crate::object::default_true")]
    pub restore_settings: bool,
}

fn default_scope() -> RenderScope {
    RenderScope::Current
}

impl Validate for ExecuteRender {
    fn validate(&self) -> Result<()> {
        self.settings.validate()?;
        if let RenderScope::Range { start, end, step } = self.scope {
            check_frame_range(start, end)?;
            if step == 0 {
                return Err(BlenderError::invalid_argument("`step` must be at least 1."));
            }
            let frames = ((end - start) / step.max(1) as i32) + 1;
            if frames > 10_000 {
                return Err(BlenderError::invalid_argument(format!(
                    "That range renders {frames} frames in one request. Render in chunks so progress is observable and cancellable."
                ))
                .with_detail("frames", frames));
            }
        }
        if let Some(name) = &self.name {
            check_artifact_name(name)?;
        }
        Ok(())
    }
}

/// `render.viewport_screenshot`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ViewportScreenshot {
    /// Render through the active camera rather than the user's viewpoint.
    #[serde(default)]
    pub camera_view: bool,
    /// Camera to use with `camera_view`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// `SOLID`, `MATERIAL`, `RENDERED` or `WIREFRAME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Validate for ViewportScreenshot {
    fn validate(&self) -> Result<()> {
        for (value, field) in [(self.width, "width"), (self.height, "height")] {
            if let Some(v) = value
                && (v == 0 || v > 8192)
            {
                return Err(BlenderError::invalid_argument(format!(
                    "`{field}` must be between 1 and 8192, got {v}."
                )));
            }
        }
        if let Some(shading) = &self.shading {
            const MODES: [&str; 4] = ["SOLID", "MATERIAL", "RENDERED", "WIREFRAME"];
            if !MODES.contains(&shading.as_str()) {
                return Err(BlenderError::invalid_enum(
                    "shading",
                    shading.clone(),
                    MODES,
                ));
            }
        }
        if self.camera.is_some() && !self.camera_view {
            return Err(BlenderError::invalid_argument(
                "`camera` requires `camera_view: true`.",
            ));
        }
        if let Some(name) = &self.name {
            check_artifact_name(name)?;
        }
        Ok(())
    }
}

/// A file the server produced and now owns.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Artifact {
    pub artifact_id: ArtifactId,
    /// Path inside a managed root. Absolute, but always under a root the server
    /// controls.
    pub path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// SHA-256 of the file contents, when the server computed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Reject a caller-supplied artifact base name that is not a plain filename.
///
/// This is the first of two defences: the server also joins and canonicalises
/// against the managed root. Rejecting separators here gives a clear error
/// instead of a confusing path-escape one.
pub fn check_artifact_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(BlenderError::invalid_argument("`name` must not be empty."));
    }
    if name.len() > 100 {
        return Err(BlenderError::invalid_argument(
            "`name` must be 100 characters or fewer.",
        ));
    }
    if name.contains(['/', '\\', ':', '\0']) || name.contains("..") {
        return Err(BlenderError::new(
            crate::ErrorCode::InvalidPath,
            "`name` is a filename, not a path: separators and `..` are not allowed.",
        )
        .with_detail("name", name));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
    {
        return Err(BlenderError::new(
            crate::ErrorCode::InvalidPath,
            "`name` may only contain letters, digits, spaces, dots, dashes and underscores.",
        )
        .with_detail("name", name));
    }
    Ok(())
}

/// `batch.render_cameras`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderCameras {
    /// Cameras to render from, in order. Empty renders every camera.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<ObjectRef>,
    #[serde(default = "default_scope")]
    pub scope: RenderScope,
    /// Prefix for the generated filenames. The camera name is appended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_prefix: Option<String>,
    #[serde(default, flatten)]
    pub settings: RenderSettings,
    /// Carry on after a camera fails instead of aborting the batch.
    #[serde(default = "crate::object::default_true")]
    pub continue_on_error: bool,
}

impl Validate for RenderCameras {
    fn validate(&self) -> Result<()> {
        self.settings.validate()?;
        if let Some(prefix) = &self.name_prefix {
            check_artifact_name(prefix)?;
        }
        Ok(())
    }
}

/// `batch.turntable`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Turntable {
    /// Object to spin around. The object itself is never rotated -- an empty is
    /// created and the camera is parented to it, so the scene is left as it was.
    pub target: ObjectRef,
    /// Camera to use. A framed one is created if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ObjectRef>,
    #[serde(default = "default_turntable_frames")]
    pub frames: u32,
    #[serde(default = "default_full_turn")]
    pub degrees: f64,
    #[serde(default = "default_turntable_axis")]
    pub axis: crate::math::Axis,
    /// Render the frames now, rather than only setting the animation up.
    #[serde(default)]
    pub render: bool,
    #[serde(default, flatten)]
    pub settings: RenderSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_turntable_frames() -> u32 {
    120
}
fn default_full_turn() -> f64 {
    360.0
}
fn default_turntable_axis() -> crate::math::Axis {
    crate::math::Axis::Z
}

impl Validate for Turntable {
    fn validate(&self) -> Result<()> {
        self.settings.validate()?;
        if self.frames == 0 || self.frames > 3600 {
            return Err(BlenderError::invalid_argument(format!(
                "`frames` must be between 1 and 3600, got {}.",
                self.frames
            )));
        }
        check_positive(self.degrees.abs(), "degrees")?;
        if let Some(name) = &self.name {
            check_artifact_name(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_jpeg_is_rejected() {
        let settings = RenderSettings {
            transparent_background: Some(true),
            format: Some(ImageFormat::Jpeg),
            ..Default::default()
        };
        assert!(settings.validate().is_err());

        let settings = RenderSettings {
            format: Some(ImageFormat::Png),
            ..settings
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn zero_resolution_is_rejected() {
        let settings = RenderSettings {
            resolution_x: Some(0),
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn artifact_names_cannot_escape_the_managed_root() {
        assert!(check_artifact_name("hero_shot").is_ok());
        assert!(check_artifact_name("../../etc/passwd").is_err());
        assert!(check_artifact_name("C:\\Windows\\system32").is_err());
        assert!(check_artifact_name("shot/01").is_err());
        assert!(check_artifact_name("").is_err());
    }

    #[test]
    fn huge_frame_ranges_are_refused() {
        let params = ExecuteRender {
            camera: None,
            scope: RenderScope::Range {
                start: 1,
                end: 50_000,
                step: 1,
            },
            name: None,
            settings: RenderSettings::default(),
            restore_settings: true,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn eevee_maps_to_the_current_identifier_first() {
        assert_eq!(RenderEngine::Eevee.candidates()[0], "BLENDER_EEVEE_NEXT");
        assert!(RenderEngine::Eevee.candidates().contains(&"BLENDER_EEVEE"));
    }

    #[test]
    fn screenshot_camera_requires_camera_view() {
        let params = ViewportScreenshot {
            camera: Some(ObjectRef::name("Camera")),
            ..Default::default()
        };
        assert!(params.validate().is_err());
    }
}

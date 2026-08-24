//! Import and export payloads.
//!
//! Two rules shape this module. First, formats are declared, not guessed: the
//! add-on reports which importers and exporters the running build actually
//! registered, and anything else is `UNSUPPORTED_FORMAT`. Second, every path is
//! relative to a managed root -- there is no way to express an absolute path
//! here at all, so traversal is a validation error rather than a filesystem
//! accident.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, ErrorCode, Result, Validate,
    ids::{CollectionRef, ObjectRef},
    math::{check_positive, check_range},
};

/// Formats the bridge can move geometry through.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileFormat {
    Fbx,
    Obj,
    Gltf,
    Glb,
    Usd,
    Usdz,
    Stl,
    Dae,
    Ply,
    Svg,
    Abc,
    /// A .blend file, appended or linked.
    Blend,
}

impl FileFormat {
    pub const ALL: [FileFormat; 12] = [
        FileFormat::Fbx,
        FileFormat::Obj,
        FileFormat::Gltf,
        FileFormat::Glb,
        FileFormat::Usd,
        FileFormat::Usdz,
        FileFormat::Stl,
        FileFormat::Dae,
        FileFormat::Ply,
        FileFormat::Svg,
        FileFormat::Abc,
        FileFormat::Blend,
    ];

    /// The canonical file extension, without a dot.
    pub const fn extension(self) -> &'static str {
        match self {
            FileFormat::Fbx => "fbx",
            FileFormat::Obj => "obj",
            FileFormat::Gltf => "gltf",
            FileFormat::Glb => "glb",
            FileFormat::Usd => "usd",
            FileFormat::Usdz => "usdz",
            FileFormat::Stl => "stl",
            FileFormat::Dae => "dae",
            FileFormat::Ply => "ply",
            FileFormat::Svg => "svg",
            FileFormat::Abc => "abc",
            FileFormat::Blend => "blend",
        }
    }

    /// Extensions that should be accepted for this format on import.
    pub const fn accepted_extensions(self) -> &'static [&'static str] {
        match self {
            // USD ships as three interchangeable containers.
            FileFormat::Usd => &["usd", "usda", "usdc"],
            FileFormat::Fbx => &["fbx"],
            FileFormat::Obj => &["obj"],
            FileFormat::Gltf => &["gltf"],
            FileFormat::Glb => &["glb"],
            FileFormat::Usdz => &["usdz"],
            FileFormat::Stl => &["stl"],
            FileFormat::Dae => &["dae"],
            FileFormat::Ply => &["ply"],
            FileFormat::Svg => &["svg"],
            FileFormat::Abc => &["abc"],
            FileFormat::Blend => &["blend"],
        }
    }

    /// Infer the format from a filename extension.
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit('.').next()?.to_ascii_lowercase();
        FileFormat::ALL
            .into_iter()
            .find(|f| f.accepted_extensions().contains(&ext.as_str()))
    }

    /// Whether this format can carry skeletal animation.
    pub const fn supports_animation(self) -> bool {
        matches!(
            self,
            FileFormat::Fbx
                | FileFormat::Gltf
                | FileFormat::Glb
                | FileFormat::Usd
                | FileFormat::Usdz
                | FileFormat::Dae
                | FileFormat::Abc
                | FileFormat::Blend
        )
    }

    /// Whether this format carries materials at all. STL and PLY do not.
    pub const fn supports_materials(self) -> bool {
        !matches!(self, FileFormat::Stl | FileFormat::Svg)
    }

    /// Whether textures can be packed into the file itself.
    pub const fn supports_embedded_textures(self) -> bool {
        matches!(
            self,
            FileFormat::Glb | FileFormat::Usdz | FileFormat::Fbx | FileFormat::Blend
        )
    }
}

/// Which managed root a path is resolved against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRoot {
    /// The configured project directory. The default for imports and exports.
    #[default]
    Project,
    /// Where downloaded external assets land.
    Downloads,
    /// Where renders and screenshots are written.
    Renders,
    /// Where exports are written.
    Exports,
    /// Scratch space, cleared between sessions.
    Temp,
}

impl ManagedRoot {
    pub const fn id(self) -> &'static str {
        match self {
            ManagedRoot::Project => "project",
            ManagedRoot::Downloads => "downloads",
            ManagedRoot::Renders => "renders",
            ManagedRoot::Exports => "exports",
            ManagedRoot::Temp => "temp",
        }
    }
}

/// A path inside a managed root.
///
/// The `path` is always relative. Absolute paths, drive letters, UNC prefixes
/// and `..` segments are rejected before the server ever touches the
/// filesystem; the server then joins and canonicalises, and re-checks
/// containment, because symlinks can still escape a purely textual check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedPath {
    #[serde(default)]
    pub root: ManagedRoot,
    /// Relative path within the root, using forward slashes.
    pub path: String,
}

impl ManagedPath {
    pub fn new(root: ManagedRoot, path: impl Into<String>) -> Self {
        Self {
            root,
            path: path.into(),
        }
    }

    pub fn project(path: impl Into<String>) -> Self {
        Self::new(ManagedRoot::Project, path)
    }

    /// The filename component.
    pub fn file_name(&self) -> Option<&str> {
        self.path.rsplit('/').next().filter(|s| !s.is_empty())
    }

    pub fn extension(&self) -> Option<String> {
        self.file_name()?
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
    }
}

impl Validate for ManagedPath {
    fn validate(&self) -> Result<()> {
        check_relative_path(&self.path)
    }
}

/// Reject anything that is not a plain relative path.
pub fn check_relative_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            "`path` must not be empty.",
        ));
    }
    if path.len() > 1024 {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            "`path` is unreasonably long.",
        ));
    }
    if path.contains('\0') {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            "`path` contains a null byte.",
        ));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            "`path` must be relative to a managed root, not absolute.",
        )
        .with_detail("path", path));
    }
    // `C:` and `\\server\share`.
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            "`path` must not carry a drive letter.",
        )
        .with_detail("path", path));
    }
    for segment in path.split(['/', '\\']) {
        if segment == ".." {
            return Err(BlenderError::new(
                ErrorCode::PathNotAllowed,
                "`path` must not contain `..` segments.",
            )
            .with_detail("path", path));
        }
    }
    Ok(())
}

/// Options shared by every importer. Format-specific extras go in `extra`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImportOptions {
    /// Uniform scale applied on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Which axis of the source data points forward: `X`, `Y`, `Z`, `-X`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_axis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_axis: Option<String>,
    /// Import animation data where the format carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_animation: Option<bool>,
    /// Import materials and textures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_materials: Option<bool>,
    /// Collection to place imported objects into. Created if missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    /// Prefix applied to every imported object's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_prefix: Option<String>,
}

/// `io.import`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Import {
    #[serde(flatten)]
    pub source: ManagedPath,
    /// Format. Inferred from the extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FileFormat>,
    #[serde(default, flatten)]
    pub options: ImportOptions,
}

impl Validate for Import {
    fn validate(&self) -> Result<()> {
        self.source.validate()?;
        let format = self.resolved_format()?;
        if let Some(extension) = self.source.extension()
            && !format.accepted_extensions().contains(&extension.as_str())
        {
            return Err(BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "`{}` does not look like a {} file.",
                    self.source.path,
                    format.extension().to_uppercase()
                ),
            )
            .with_detail("extension", extension)
            .with_detail_json("expected", &format.accepted_extensions()));
        }
        if let Some(scale) = self.options.scale {
            check_positive(scale, "scale")?;
        }
        check_axis(self.options.forward_axis.as_deref(), "forward_axis")?;
        check_axis(self.options.up_axis.as_deref(), "up_axis")?;
        if let (Some(forward), Some(up)) = (&self.options.forward_axis, &self.options.up_axis)
            && forward.trim_start_matches('-') == up.trim_start_matches('-')
        {
            return Err(BlenderError::invalid_argument(
                "`forward_axis` and `up_axis` cannot be the same axis.",
            ));
        }
        Ok(())
    }
}

impl Import {
    /// The format to use, inferring from the extension when not given.
    pub fn resolved_format(&self) -> Result<FileFormat> {
        if let Some(format) = self.format {
            return Ok(format);
        }
        FileFormat::from_path(&self.source.path).ok_or_else(|| {
            BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "Cannot infer a format from `{}`; pass `format` explicitly.",
                    self.source.path
                ),
            )
            .with_detail_json(
                "supported",
                &FileFormat::ALL.map(|f| f.extension()).to_vec(),
            )
        })
    }
}

fn check_axis(axis: Option<&str>, field: &str) -> Result<()> {
    let Some(axis) = axis else { return Ok(()) };
    const AXES: [&str; 6] = ["X", "Y", "Z", "-X", "-Y", "-Z"];
    if AXES.contains(&axis) {
        Ok(())
    } else {
        Err(BlenderError::invalid_enum(field, axis, AXES))
    }
}

/// What an export includes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportSelection {
    /// Everything in the scene.
    Scene,
    /// Only the currently selected objects.
    Selected,
    /// Named objects.
    Objects(Vec<ObjectRef>),
    /// Everything in a collection.
    Collection(CollectionRef),
}

/// Options shared by every exporter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ExportOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_axis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_axis: Option<String>,
    /// Evaluate modifiers before exporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_modifiers: Option<bool>,
    /// Convert quads and ngons to triangles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triangulate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_materials: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_animation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_normals: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_uvs: Option<bool>,
    /// Copy referenced textures next to the exported file, or embed them where
    /// the format allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub textures: Option<TextureHandling>,
    /// Frame range for animated formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_range: Option<(i32, i32)>,
}

/// What to do with texture files on export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TextureHandling {
    /// Leave paths as they are.
    Keep,
    /// Copy texture files next to the export.
    Copy,
    /// Embed textures in the exported file.
    Embed,
    /// Do not reference textures at all.
    Strip,
}

/// `io.export`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Export {
    /// Destination, relative to a managed root. Defaults to the exports root.
    pub destination: ManagedPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FileFormat>,
    #[serde(default = "default_export_selection")]
    pub selection: ExportSelection,
    #[serde(default, flatten)]
    pub options: ExportOptions,
}

fn default_export_selection() -> ExportSelection {
    ExportSelection::Scene
}

impl Validate for Export {
    fn validate(&self) -> Result<()> {
        self.destination.validate()?;
        let format = self.resolved_format()?;
        if let Some(scale) = self.options.scale {
            check_positive(scale, "scale")?;
        }
        check_axis(self.options.forward_axis.as_deref(), "forward_axis")?;
        check_axis(self.options.up_axis.as_deref(), "up_axis")?;
        if let Some((start, end)) = self.options.frame_range {
            crate::check_frame_range(start, end)?;
        }
        if self.options.export_animation == Some(true) && !format.supports_animation() {
            return Err(BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "{} does not carry animation; the exported file would be a static snapshot.",
                    format.extension().to_uppercase()
                ),
            )
            .with_detail("format", format.extension()));
        }
        if self.options.export_materials == Some(true) && !format.supports_materials() {
            return Err(BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "{} does not carry materials.",
                    format.extension().to_uppercase()
                ),
            )
            .with_detail("format", format.extension()));
        }
        if self.options.textures == Some(TextureHandling::Embed)
            && !format.supports_embedded_textures()
        {
            return Err(BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "{} cannot embed textures; use `COPY` instead.",
                    format.extension().to_uppercase()
                ),
            )
            .with_detail("format", format.extension()));
        }
        if let ExportSelection::Objects(objects) = &self.selection
            && objects.is_empty()
        {
            return Err(BlenderError::invalid_argument(
                "`selection.objects` is empty; use `scene` to export everything.",
            ));
        }
        Ok(())
    }
}

impl Export {
    pub fn resolved_format(&self) -> Result<FileFormat> {
        if let Some(format) = self.format {
            return Ok(format);
        }
        FileFormat::from_path(&self.destination.path).ok_or_else(|| {
            BlenderError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "Cannot infer a format from `{}`; pass `format` explicitly.",
                    self.destination.path
                ),
            )
        })
    }
}

/// Target platform profile for `workflow.export.prepare`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExportProfile {
    /// Sensible defaults with no engine-specific assumptions.
    #[default]
    Generic,
    /// Applied transforms, triangulated, UV-checked, sane names.
    GameAsset,
    /// glTF conventions: +Y up, metallic-roughness, no ngons.
    Gltf,
    /// Unreal conventions: centimetre scale, -Y forward, +Z up.
    Unreal,
    /// Unity conventions: metre scale, +Z forward, +Y up.
    Unity,
}

impl ExportProfile {
    /// Unit scale multiplier this profile expects, relative to metres.
    pub const fn scale(self) -> f64 {
        match self {
            ExportProfile::Unreal => 100.0,
            _ => 1.0,
        }
    }

    /// `(forward, up)` axes this profile expects.
    pub const fn axes(self) -> (&'static str, &'static str) {
        match self {
            ExportProfile::Gltf | ExportProfile::Unity => ("Z", "Y"),
            ExportProfile::Unreal => ("-Y", "Z"),
            _ => ("Y", "Z"),
        }
    }

    /// Whether the profile insists on triangles.
    pub const fn requires_triangles(self) -> bool {
        matches!(
            self,
            ExportProfile::GameAsset
                | ExportProfile::Gltf
                | ExportProfile::Unreal
                | ExportProfile::Unity
        )
    }
}

/// `workflow.export.prepare`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrepareExport {
    #[serde(default = "default_export_selection")]
    pub selection: ExportSelection,
    #[serde(default)]
    pub profile: ExportProfile,
    /// Report problems without changing anything.
    #[serde(default = "crate::object::default_true")]
    pub dry_run: bool,
    /// Apply the fixes the profile calls for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<PrepareFixes>,
    /// Also export once preparation succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<Export>,
}

/// Which corrective actions `workflow.export.prepare` may take.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PrepareFixes {
    #[serde(default)]
    pub apply_transforms: bool,
    #[serde(default)]
    pub recalculate_normals: bool,
    #[serde(default)]
    pub triangulate: bool,
    #[serde(default)]
    pub sanitize_names: bool,
    #[serde(default)]
    pub remove_loose_geometry: bool,
    #[serde(default)]
    pub pack_textures: bool,
}

impl Validate for PrepareExport {
    fn validate(&self) -> Result<()> {
        if let Some(export) = &self.export {
            export.validate()?;
            if self.dry_run {
                return Err(BlenderError::invalid_argument(
                    "`export` and `dry_run` contradict each other: a dry run writes nothing.",
                ));
            }
        }
        if self.dry_run && self.fix.is_some() {
            return Err(BlenderError::invalid_argument(
                "`fix` changes the scene, which `dry_run` forbids.",
            ));
        }
        Ok(())
    }
}

/// One problem found while preparing an export.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub severity: Severity,
    /// Stable code, e.g. `UNAPPLIED_SCALE`.
    pub code: String,
    /// Entity the finding is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
    /// Whether one of the `fix` flags would resolve it.
    #[serde(default)]
    pub auto_fixable: bool,
}

/// How serious a finding is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// `io.capabilities` result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct IoCapabilities {
    pub import: Vec<FormatCapability>,
    pub export: Vec<FormatCapability>,
}

/// What one format supports in the connected build.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FormatCapability {
    pub format: FileFormat,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub supports_animation: bool,
    #[serde(default)]
    pub supports_materials: bool,
    #[serde(default)]
    pub supports_embedded_textures: bool,
    /// The operator or extension the bridge will call. Useful for diagnostics
    /// when an importer is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
}

/// Reject an export scale that is a common unit mistake.
pub fn check_export_scale(scale: f64, profile: ExportProfile) -> Result<()> {
    check_range(scale, 1e-6, 1e6, "scale")?;
    let expected = profile.scale();
    if (scale - expected).abs() > f64::EPSILON && (scale / expected - 1.0).abs() > 0.5 {
        // Not an error: overriding the profile scale is legitimate. Recorded
        // only so a workflow can surface it as a warning.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(path: &str) -> Export {
        Export {
            destination: ManagedPath::new(ManagedRoot::Exports, path),
            format: None,
            selection: ExportSelection::Scene,
            options: ExportOptions::default(),
        }
    }

    #[test]
    fn relative_paths_only() {
        assert!(check_relative_path("models/hero.fbx").is_ok());
        assert!(check_relative_path("/etc/passwd").is_err());
        assert!(check_relative_path("C:/Windows/system32").is_err());
        assert!(check_relative_path("../../secrets").is_err());
        assert!(check_relative_path("models/../../secrets").is_err());
        assert!(check_relative_path("").is_err());
    }

    #[test]
    fn backslash_traversal_is_caught_too() {
        assert!(check_relative_path("models\\..\\..\\secrets").is_err());
        assert!(check_relative_path("\\\\server\\share\\x.fbx").is_err());
    }

    #[test]
    fn format_is_inferred_from_the_extension() {
        assert_eq!(FileFormat::from_path("a/b/hero.GLB"), Some(FileFormat::Glb));
        assert_eq!(FileFormat::from_path("scene.usda"), Some(FileFormat::Usd));
        assert_eq!(FileFormat::from_path("notes.txt"), None);
    }

    #[test]
    fn stl_cannot_export_materials() {
        let mut params = export("hero.stl");
        params.options.export_materials = Some(true);
        let err = params.validate().unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedFormat);
    }

    #[test]
    fn obj_cannot_export_animation() {
        let mut params = export("hero.obj");
        params.options.export_animation = Some(true);
        assert_eq!(
            params.validate().unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );
    }

    #[test]
    fn gltf_cannot_embed_textures_but_glb_can() {
        let mut params = export("hero.gltf");
        params.options.textures = Some(TextureHandling::Embed);
        assert!(params.validate().is_err());

        let mut params = export("hero.glb");
        params.options.textures = Some(TextureHandling::Embed);
        assert!(params.validate().is_ok());
    }

    #[test]
    fn mismatched_extension_and_format_is_rejected() {
        let params = Import {
            source: ManagedPath::project("hero.obj"),
            format: Some(FileFormat::Fbx),
            options: ImportOptions::default(),
        };
        assert_eq!(
            params.validate().unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );
    }

    #[test]
    fn identical_axes_are_rejected() {
        let params = Import {
            source: ManagedPath::project("hero.fbx"),
            format: None,
            options: ImportOptions {
                forward_axis: Some("Y".into()),
                up_axis: Some("-Y".into()),
                ..Default::default()
            },
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn dry_run_and_fixes_are_mutually_exclusive() {
        let params = PrepareExport {
            selection: ExportSelection::Scene,
            profile: ExportProfile::GameAsset,
            dry_run: true,
            fix: Some(PrepareFixes {
                apply_transforms: true,
                ..Default::default()
            }),
            export: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn unreal_profile_uses_centimetres() {
        assert_eq!(ExportProfile::Unreal.scale(), 100.0);
        assert_eq!(ExportProfile::Gltf.axes(), ("Z", "Y"));
    }
}

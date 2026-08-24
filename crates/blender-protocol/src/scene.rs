//! Scene-level payloads: settings, world, summary, selection and utilities.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Result, Validate, check_frame_range, check_name,
    ids::{ObjectId, ObjectRef, SceneId},
    math::{Color4, Finite, check_positive},
};

/// `scene.summary` -- the compact state snapshot a model should read first.
///
/// Everything here is O(number of objects) to produce and small enough to sit
/// in a context window: no geometry, no node graphs, no per-object detail.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SceneSummary {
    pub revision: u64,
    pub scene: String,
    pub scene_id: SceneId,
    pub objects: ObjectCounts,
    pub materials: u32,
    pub collections: u32,
    pub images: u32,
    pub actions: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_camera: Option<String>,
    pub frame_current: i32,
    pub frame_start: i32,
    pub frame_end: i32,
    pub fps: f64,
    pub render_engine: String,
    pub unit_scale: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filepath: Option<String>,
    #[serde(default)]
    pub unsaved_changes: bool,
}

/// Object population by type.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
pub struct ObjectCounts {
    pub total: u32,
    #[serde(default)]
    pub mesh: u32,
    #[serde(default)]
    pub light: u32,
    #[serde(default)]
    pub camera: u32,
    #[serde(default)]
    pub armature: u32,
    #[serde(default)]
    pub curve: u32,
    #[serde(default)]
    pub empty: u32,
    #[serde(default)]
    pub other: u32,
}

/// `scene.settings.update`. Every field is optional; omitted fields are left
/// exactly as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SceneSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_start: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_end: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_current: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Metres per Blender unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_scale: Option<f64>,
    /// `NONE`, `METRIC` or `IMPERIAL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_system: Option<UnitSystem>,
    /// Object to use as the scene's active camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_camera: Option<ObjectRef>,
    /// 3D cursor position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_location: Option<crate::math::Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity: Option<crate::math::Vec3>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UnitSystem {
    None,
    Metric,
    Imperial,
}

impl Validate for SceneSettings {
    fn validate(&self) -> Result<()> {
        if let (Some(start), Some(end)) = (self.frame_start, self.frame_end) {
            check_frame_range(start, end)?;
        }
        if let Some(fps) = self.fps {
            check_positive(fps, "fps")?;
            if fps > 240.0 {
                return Err(BlenderError::invalid_argument(format!(
                    "`fps` of {fps} is outside the range Blender handles sensibly (1-240)."
                )));
            }
        }
        if let Some(scale) = self.unit_scale {
            check_positive(scale, "unit_scale")?;
        }
        self.cursor_location.check_finite("cursor_location")?;
        self.gravity.check_finite("gravity")?;
        Ok(())
    }
}

/// World (environment) settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorldSettings {
    /// Flat background colour. Ignored when `hdri` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color4>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
    /// Managed path or artifact id of an equirectangular HDRI to use as the
    /// environment texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdri: Option<String>,
    /// Environment rotation about Z, in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_z: Option<f64>,
    /// Render the world as transparent film while still lighting the scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparent: Option<bool>,
}

impl Validate for WorldSettings {
    fn validate(&self) -> Result<()> {
        if let Some(color) = self.color {
            color.check_finite("color")?;
        }
        if let Some(strength) = self.strength {
            crate::math::check_non_negative(strength, "strength")?;
        }
        if let Some(rotation) = self.rotation_z {
            crate::math::check_scalar_finite(rotation, "rotation_z")?;
        }
        Ok(())
    }
}

/// How a selection operation combines with the current selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SelectionMode {
    /// Replace the selection entirely.
    #[default]
    Set,
    Add,
    Remove,
}

/// `selection.set` / `selection.add` / `selection.remove`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SelectionUpdate {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    #[serde(default)]
    pub mode: SelectionMode,
    /// Object to make active. Must be in the resulting selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ObjectRef>,
}

impl Validate for SelectionUpdate {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() && self.active.is_none() {
            return Err(BlenderError::invalid_argument(
                "Provide `objects`, `active`, or both. Use `selection.clear` to deselect everything.",
            ));
        }
        Ok(())
    }
}

/// Current selection state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SelectionState {
    #[serde(default)]
    pub selected: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ObjectId>,
    #[serde(default)]
    pub names: Vec<String>,
    pub mode: String,
}

/// Which cleanup passes `scene.cleanup` should run. Every pass is opt-in:
/// a single `cleanup: true` that silently deletes data is exactly the kind of
/// operation that loses someone's afternoon.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CleanupOptions {
    /// Purge data-blocks with zero users.
    #[serde(default)]
    pub purge_orphans: bool,
    /// Delete collections that contain no objects and no child collections.
    #[serde(default)]
    pub remove_empty_collections: bool,
    /// Delete loose vertices and edges from mesh objects.
    #[serde(default)]
    pub remove_loose_geometry: bool,
    /// Remove material slots that reference nothing.
    #[serde(default)]
    pub remove_unused_material_slots: bool,
    /// Merge materials whose names differ only by a `.001`-style suffix and
    /// whose settings match.
    #[serde(default)]
    pub merge_duplicate_materials: bool,
    /// Recalculate mesh normals outward.
    #[serde(default)]
    pub recalculate_normals: bool,
    /// Remove modifiers whose required target object is missing.
    #[serde(default)]
    pub remove_invalid_modifiers: bool,
    /// Report what would change without changing anything.
    #[serde(default)]
    pub dry_run: bool,
}

impl Validate for CleanupOptions {
    fn validate(&self) -> Result<()> {
        let any = self.purge_orphans
            || self.remove_empty_collections
            || self.remove_loose_geometry
            || self.remove_unused_material_slots
            || self.merge_duplicate_materials
            || self.recalculate_normals
            || self.remove_invalid_modifiers;
        if !any {
            return Err(BlenderError::invalid_argument(
                "`scene.cleanup` does nothing unless at least one pass is enabled. Set the passes you want explicitly.",
            ));
        }
        Ok(())
    }
}

/// One rename transformation, applied in the order the fields are listed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RenamePattern {
    /// Literal substring to replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<String>,
    /// Regular expression alternative to `find`. Evaluated in Rust, never in
    /// Blender, and rejected if it is not a valid pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Strip this many characters from the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_start: Option<u32>,
    /// Strip this many characters from the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case: Option<CaseMode>,
    /// Append a zero-padded counter, starting at this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_start: Option<u32>,
    /// Digits to pad the counter to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_padding: Option<u32>,
    /// Where the counter goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_position: Option<NumberPosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaseMode {
    Upper,
    Lower,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NumberPosition {
    Prefix,
    Suffix,
}

impl Validate for RenamePattern {
    fn validate(&self) -> Result<()> {
        if self.find.is_some() && self.regex.is_some() {
            return Err(BlenderError::invalid_argument(
                "Set `find` or `regex`, not both.",
            ));
        }
        if (self.find.is_some() || self.regex.is_some()) && self.replace.is_none() {
            return Err(BlenderError::invalid_argument(
                "`replace` is required alongside `find`/`regex` (use an empty string to delete).",
            ));
        }
        if let Some(padding) = self.number_padding
            && padding > 10
        {
            return Err(BlenderError::invalid_argument(
                "`number_padding` above 10 exceeds Blender's usable name length.",
            ));
        }
        if let Some(prefix) = &self.prefix {
            check_name(prefix, "prefix")?;
        }
        if let Some(suffix) = &self.suffix {
            check_name(suffix, "suffix")?;
        }
        let any = self.find.is_some()
            || self.regex.is_some()
            || self.prefix.is_some()
            || self.suffix.is_some()
            || self.strip_start.is_some()
            || self.strip_end.is_some()
            || self.case.is_some()
            || self.number_start.is_some();
        if !any {
            return Err(BlenderError::invalid_argument(
                "The rename pattern is empty; nothing would change.",
            ));
        }
        Ok(())
    }
}

/// Scene-wide statistics, deliberately cheap to compute.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SceneStatistics {
    pub objects: ObjectCounts,
    pub vertices: u64,
    pub edges: u64,
    pub faces: u64,
    pub triangles: u64,
    pub materials: u32,
    pub images: u32,
    pub collections: u32,
    pub modifiers: u32,
    pub hidden_objects: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_memory_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_requires_an_explicit_pass() {
        assert!(CleanupOptions::default().validate().is_err());
        let opts = CleanupOptions {
            purge_orphans: true,
            ..Default::default()
        };
        assert!(opts.validate().is_ok());
    }

    #[test]
    fn inverted_frame_range_is_rejected() {
        let settings = SceneSettings {
            frame_start: Some(100),
            frame_end: Some(1),
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rename_pattern_needs_a_replacement() {
        let pattern = RenamePattern {
            find: Some("old".into()),
            ..Default::default()
        };
        assert!(pattern.validate().is_err());
        let pattern = RenamePattern {
            find: Some("old".into()),
            replace: Some(String::new()),
            ..Default::default()
        };
        assert!(pattern.validate().is_ok());
    }

    #[test]
    fn empty_selection_update_is_rejected() {
        assert!(SelectionUpdate::default().validate().is_err());
    }
}

/// `scene.batch_rename`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatchRename {
    /// Which data-blocks to rename.
    #[serde(default = "default_rename_kind")]
    pub kind: RenameKind,
    /// Specific data-blocks to rename. Empty renames everything of that kind
    /// that passes `name_contains`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Case-insensitive substring filter, when `targets` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(flatten)]
    pub pattern: RenamePattern,
    /// Report the proposed renames without applying them.
    #[serde(default)]
    pub dry_run: bool,
}

/// Which collection `scene.batch_rename` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenameKind {
    #[default]
    Objects,
    Materials,
    Collections,
    Meshes,
    Actions,
    Images,
}

fn default_rename_kind() -> RenameKind {
    RenameKind::Objects
}

/// `scene.apply_transforms`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplySceneTransforms {
    /// Objects to apply. Empty applies to every transformable object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    #[serde(default)]
    pub location: bool,
    #[serde(default)]
    pub rotation: bool,
    #[serde(default = "crate::object::default_true")]
    pub scale: bool,
}

/// `scene.mesh_analysis`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SceneMeshAnalysis {
    /// Objects to analyse. Empty analyses every mesh in the scene.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
}

/// `scene.purge_orphans`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PurgeOrphans {
    /// List what would go without removing anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// `scene.find_duplicates`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct FindDuplicates {
    /// How close two origins must be to count as the same place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
}

impl Validate for SceneMeshAnalysis {}
impl Validate for PurgeOrphans {}

impl Validate for BatchRename {
    fn validate(&self) -> Result<()> {
        self.pattern.validate()?;
        if let Some(regex) = &self.pattern.regex
            && regex.len() > 200
        {
            return Err(BlenderError::invalid_argument("`regex` is too long."));
        }
        Ok(())
    }
}

impl Validate for ApplySceneTransforms {
    fn validate(&self) -> Result<()> {
        if !(self.location || self.rotation || self.scale) {
            return Err(BlenderError::invalid_argument(
                "Enable at least one of location, rotation or scale.",
            ));
        }
        Ok(())
    }
}

impl Validate for FindDuplicates {
    fn validate(&self) -> Result<()> {
        if let Some(tolerance) = self.tolerance {
            crate::math::check_positive(tolerance, "tolerance")?;
        }
        Ok(())
    }
}

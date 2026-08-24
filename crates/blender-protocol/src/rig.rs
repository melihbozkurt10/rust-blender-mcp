//! Rigging and rig diagnostics payloads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{ArmatureId, ArmatureRef, BoneId, ObjectRef},
    math::{Finite, Vec3, check_non_negative, check_range},
};

/// `rig.armature.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateArmature {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec3>,
    /// Bones to create immediately, parents before children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<BoneSpec>,
    /// Show bone axes and names in the viewport.
    #[serde(default)]
    pub show_names: bool,
    /// `OCTAHEDRAL`, `STICK`, `BBONE`, `ENVELOPE` or `WIRE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_type: Option<String>,
}

impl Validate for CreateArmature {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.location.check_finite("location")?;
        let mut seen = std::collections::BTreeSet::new();
        for bone in &self.bones {
            bone.validate()?;
            if !seen.insert(bone.name.as_str()) {
                return Err(BlenderError::invalid_argument(format!(
                    "`{}` appears twice in `bones`; bone names must be unique.",
                    bone.name
                ))
                .with_detail("bone", bone.name.clone()));
            }
        }
        // Parents must be declared before the bones that reference them,
        // because the bridge creates them in order.
        let mut created = std::collections::BTreeSet::new();
        for bone in &self.bones {
            if let Some(parent) = &bone.parent
                && !created.contains(parent.as_str())
            {
                return Err(BlenderError::invalid_argument(format!(
                    "`{}` is parented to `{parent}`, which is not defined earlier in `bones`.",
                    bone.name
                ))
                .with_detail("bone", bone.name.clone())
                .with_detail("parent", parent.clone()));
            }
            created.insert(bone.name.as_str());
        }
        Ok(())
    }
}

/// One bone to create.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BoneSpec {
    pub name: String,
    pub head: Vec3,
    pub tail: Vec3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Move the head to the parent's tail and keep it there.
    #[serde(default)]
    pub connected: bool,
    /// Roll about the bone's own axis, in radians.
    #[serde(default)]
    pub roll: f64,
    /// Include this bone in the armature deformation.
    #[serde(default = "crate::object::default_true")]
    pub deform: bool,
}

impl Validate for BoneSpec {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")?;
        self.head.check_finite("head")?;
        self.tail.check_finite("tail")?;
        crate::math::check_scalar_finite(self.roll, "roll")?;
        // Blender silently deletes zero-length bones on leaving edit mode,
        // which looks like the operation succeeded and then did nothing.
        if self.head.distance(self.tail) < 1e-6 {
            return Err(BlenderError::invalid_argument(format!(
                "Bone `{}` has zero length; Blender discards such bones.",
                self.name
            ))
            .with_detail("bone", self.name.clone()));
        }
        if let Some(parent) = &self.parent {
            check_name(parent, "parent")?;
            if parent == &self.name {
                return Err(BlenderError::invalid_argument(format!(
                    "Bone `{}` cannot be its own parent.",
                    self.name
                )));
            }
        }
        if self.connected && self.parent.is_none() {
            return Err(BlenderError::invalid_argument(format!(
                "Bone `{}` is `connected` but has no parent to connect to.",
                self.name
            )));
        }
        Ok(())
    }
}

/// `rig.bone.create` / `rig.bone.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BoneOperation {
    pub armature: ArmatureRef,
    #[serde(flatten)]
    pub bone: BoneSpec,
}

impl Validate for BoneOperation {
    fn validate(&self) -> Result<()> {
        self.bone.validate()
    }
}

/// `rig.bone.parent`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ParentBone {
    pub armature: ArmatureRef,
    pub bone: String,
    /// Parent bone name. Omit to clear the parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub connected: bool,
    /// Keep the bone's current position when reparenting.
    #[serde(default = "crate::object::default_true")]
    pub keep_transform: bool,
}

impl Validate for ParentBone {
    fn validate(&self) -> Result<()> {
        check_name(&self.bone, "bone")?;
        if let Some(parent) = &self.parent {
            check_name(parent, "parent")?;
            if parent == &self.bone {
                return Err(BlenderError::invalid_argument(
                    "A bone cannot be its own parent.",
                ));
            }
        } else if self.connected {
            return Err(BlenderError::invalid_argument(
                "`connected` requires a parent.",
            ));
        }
        Ok(())
    }
}

/// Left/right naming conventions the diagnostics understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SideConvention {
    /// `Arm.L` / `Arm.R`. Blender's own convention and the only one its
    /// symmetry tools understand.
    #[default]
    DotSuffix,
    /// `Arm_L` / `Arm_R`.
    UnderscoreSuffix,
    /// `Left Arm` / `Right Arm`.
    WordPrefix,
    /// `LeftArm` / `RightArm`.
    CamelPrefix,
}

impl SideConvention {
    /// The `(left, right)` markers for this convention.
    pub const fn markers(self) -> (&'static str, &'static str) {
        match self {
            SideConvention::DotSuffix => (".L", ".R"),
            SideConvention::UnderscoreSuffix => ("_L", "_R"),
            SideConvention::WordPrefix => ("Left ", "Right "),
            SideConvention::CamelPrefix => ("Left", "Right"),
        }
    }

    /// Whether the marker goes at the end of the name.
    pub const fn is_suffix(self) -> bool {
        matches!(
            self,
            SideConvention::DotSuffix | SideConvention::UnderscoreSuffix
        )
    }

    /// The mirrored name for a bone, or `None` if it carries no side marker.
    pub fn mirror_name(self, name: &str) -> Option<String> {
        let (left, right) = self.markers();
        if self.is_suffix() {
            if let Some(stem) = name.strip_suffix(left) {
                return Some(format!("{stem}{right}"));
            }
            if let Some(stem) = name.strip_suffix(right) {
                return Some(format!("{stem}{left}"));
            }
        } else {
            if let Some(stem) = name.strip_prefix(left) {
                return Some(format!("{right}{stem}"));
            }
            if let Some(stem) = name.strip_prefix(right) {
                return Some(format!("{left}{stem}"));
            }
        }
        None
    }
}

/// `rig.bone.mirror` / `rig.fix.mirror_bones`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MirrorBones {
    pub armature: ArmatureRef,
    /// Bones to mirror. Empty mirrors every bone carrying a side marker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<String>,
    #[serde(default)]
    pub convention: SideConvention,
    /// Which side is the source of truth.
    #[serde(default = "default_mirror_direction")]
    pub direction: MirrorDirection,
    /// Axis to mirror across.
    #[serde(default = "default_mirror_axis")]
    pub axis: crate::math::Axis,
    /// Overwrite bones that already exist on the destination side.
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MirrorDirection {
    LeftToRight,
    RightToLeft,
}

fn default_mirror_direction() -> MirrorDirection {
    MirrorDirection::LeftToRight
}

fn default_mirror_axis() -> crate::math::Axis {
    crate::math::Axis::X
}

/// `rig.vertex_group.*`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VertexGroupOperation {
    pub object: ObjectRef,
    pub group: String,
    /// Vertices to assign. Empty for create/delete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vertices: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    /// `REPLACE`, `ADD`, `SUBTRACT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mesh_revision: Option<u64>,
}

impl Validate for VertexGroupOperation {
    fn validate(&self) -> Result<()> {
        check_name(&self.group, "group")?;
        if let Some(weight) = self.weight {
            check_range(weight, 0.0, 1.0, "weight")?;
        }
        if let Some(mode) = &self.mode {
            const MODES: [&str; 3] = ["REPLACE", "ADD", "SUBTRACT"];
            if !MODES.contains(&mode.as_str()) {
                return Err(BlenderError::invalid_enum("mode", mode.clone(), MODES));
            }
        }
        Ok(())
    }
}

/// `rig.parent_mesh` / `rig.auto_weights`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BindMesh {
    pub armature: ObjectRef,
    pub meshes: Vec<ObjectRef>,
    /// How to generate weights.
    #[serde(default)]
    pub weighting: WeightingMode,
    /// Keep any existing vertex groups instead of replacing them.
    #[serde(default)]
    pub keep_existing_groups: bool,
}

/// How `rig.parent_mesh` produces weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WeightingMode {
    /// Heat-map weights from bone geometry. Blender's "with automatic weights".
    #[default]
    Automatic,
    /// Envelope-based weights.
    Envelope,
    /// Create the modifier and empty groups; weights are painted later.
    Empty,
}

impl Validate for BindMesh {
    fn validate(&self) -> Result<()> {
        if self.meshes.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`meshes` must name at least one object.",
            ));
        }
        if self.meshes.contains(&self.armature) {
            return Err(BlenderError::invalid_argument(
                "The armature cannot also be one of the meshes.",
            ));
        }
        Ok(())
    }
}

/// Constraint types the bridge exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConstraintType {
    CopyLocation,
    CopyRotation,
    CopyScale,
    CopyTransforms,
    TrackTo,
    DampedTrack,
    LockedTrack,
    StretchTo,
    Ik,
    LimitLocation,
    LimitRotation,
    LimitScale,
    ChildOf,
    Floor,
    Armature,
}

/// `rig.constraint.add` / `update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintOperation {
    /// Object carrying the constraint.
    pub object: ObjectRef,
    /// Bone carrying the constraint, for pose-bone constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    #[serde(rename = "type")]
    pub constraint_type: ConstraintType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
    /// Target bone within `target`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtarget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub influence: Option<f64>,
    /// IK chain length. 0 means the whole chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_length: Option<u32>,
    /// Additional typed properties.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<crate::node_graph::PropertyAssignment>,
}

impl ConstraintType {
    /// Whether this constraint is useless without a target.
    pub const fn requires_target(self) -> bool {
        !matches!(
            self,
            ConstraintType::LimitLocation
                | ConstraintType::LimitRotation
                | ConstraintType::LimitScale
        )
    }
}

impl Validate for ConstraintOperation {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if let Some(bone) = &self.bone {
            check_name(bone, "bone")?;
        }
        if let Some(subtarget) = &self.subtarget {
            check_name(subtarget, "subtarget")?;
        }
        if let Some(influence) = self.influence {
            check_range(influence, 0.0, 1.0, "influence")?;
        }
        if self.constraint_type.requires_target() && self.target.is_none() {
            return Err(BlenderError::invalid_argument(format!(
                "A {:?} constraint needs a `target`.",
                self.constraint_type
            ))
            .with_detail("field", "target"));
        }
        if self.constraint_type == ConstraintType::Ik && self.bone.is_none() {
            return Err(BlenderError::invalid_argument(
                "IK constraints live on a pose bone; set `bone`.",
            ));
        }
        for property in &self.properties {
            property.validate()?;
        }
        Ok(())
    }
}

/// `rig.vertex_group.normalize` / `rig.fix.normalize_weights`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NormalizeWeights {
    pub objects: Vec<ObjectRef>,
    /// Only normalise these groups. Empty means all deform groups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Leave locked groups untouched and redistribute the rest.
    #[serde(default)]
    pub lock_active: bool,
    /// Cap how many bones may influence one vertex, dropping the smallest
    /// weights. Game engines commonly require 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_influences: Option<u32>,
    #[serde(default)]
    pub dry_run: bool,
}

impl Validate for NormalizeWeights {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        if let Some(max) = self.max_influences
            && (max == 0 || max > 32)
        {
            return Err(BlenderError::invalid_argument(format!(
                "`max_influences` must be between 1 and 32, got {max}."
            )));
        }
        Ok(())
    }
}

/// `rig.fix.naming`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FixNaming {
    pub armature: ArmatureRef,
    /// Convention to convert *to*.
    #[serde(default)]
    pub convention: SideConvention,
    /// Also rename the matching vertex groups on bound meshes, so weights keep
    /// working. Renaming bones without this silently breaks deformation.
    #[serde(default = "crate::object::default_true")]
    pub rename_vertex_groups: bool,
    /// Report proposed renames without applying them.
    #[serde(default = "crate::object::default_true")]
    pub dry_run: bool,
}

/// A proposed or applied rename.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Rename {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One rig problem.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RigFinding {
    pub severity: crate::io::Severity,
    /// Stable code, e.g. `ZERO_LENGTH_BONE`.
    pub code: String,
    /// Entity the finding concerns: a bone name, a vertex group, an object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
    /// Extra numbers: counts, weights, indices.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub details: serde_json::Map<String, serde_json::Value>,
}

/// Result of a diagnostics run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RigReport {
    #[serde(default)]
    pub findings: Vec<RigFinding>,
    #[serde(default)]
    pub bone_count: u32,
    #[serde(default)]
    pub deform_bone_count: u32,
    #[serde(default)]
    pub bound_meshes: Vec<String>,
    /// Highest severity present, for a quick pass/fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worst_severity: Option<crate::io::Severity>,
}

impl RigReport {
    /// Recompute `worst_severity` from the findings.
    pub fn summarise(mut self) -> Self {
        self.worst_severity = self.findings.iter().map(|f| f.severity).max();
        self
    }
}

/// `rig.diagnostics.weights`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WeightDiagnostics {
    pub objects: Vec<ObjectRef>,
    /// Report vertices whose total weight differs from 1 by more than this.
    #[serde(default = "default_weight_tolerance")]
    pub tolerance: f64,
    /// Report vertices influenced by more bones than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_influences: Option<u32>,
    /// Cap on how many individual vertices to name in the report.
    #[serde(default = "default_sample_limit")]
    pub sample_limit: u32,
}

fn default_weight_tolerance() -> f64 {
    0.001
}
fn default_sample_limit() -> u32 {
    50
}

impl Validate for WeightDiagnostics {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        check_non_negative(self.tolerance, "tolerance")?;
        if self.sample_limit > 1000 {
            return Err(BlenderError::invalid_argument(
                "`sample_limit` above 1000 produces a report too large to be useful.",
            ));
        }
        Ok(())
    }
}

/// A bone as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BoneSummary {
    pub id: BoneId,
    pub name: String,
    pub head: Vec3,
    pub tail: Vec3,
    pub length: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub deform: bool,
    #[serde(default)]
    pub roll: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

/// An armature as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArmatureSummary {
    /// Id of the armature *object*.
    pub id: crate::ids::ObjectId,
    /// Id of the armature data-block.
    pub data_id: ArmatureId,
    pub name: String,
    pub bone_count: u32,
    #[serde(default)]
    pub deform_bone_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_bones: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bound_meshes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// `rig.bone.list` filters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListBones {
    pub armature: ArmatureRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only bones that deform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deform_only: Option<bool>,
    /// Only direct children of this bone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, flatten)]
    pub page: Page,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_bones_are_rejected() {
        let bone = BoneSpec {
            name: "Root".into(),
            head: Vec3::ZERO,
            tail: Vec3::ZERO,
            parent: None,
            connected: false,
            roll: 0.0,
            deform: true,
        };
        assert!(bone.validate().is_err());
    }

    #[test]
    fn parents_must_be_declared_first() {
        let child = BoneSpec {
            name: "Forearm".into(),
            head: Vec3::ZERO,
            tail: Vec3::new(0.0, 0.3, 0.0),
            parent: Some("UpperArm".into()),
            connected: true,
            roll: 0.0,
            deform: true,
        };
        let parent = BoneSpec {
            name: "UpperArm".into(),
            head: Vec3::new(0.0, -0.3, 0.0),
            tail: Vec3::ZERO,
            parent: None,
            connected: false,
            roll: 0.0,
            deform: true,
        };
        let out_of_order = CreateArmature {
            name: None,
            location: None,
            bones: vec![child.clone(), parent.clone()],
            show_names: false,
            display_type: None,
        };
        assert!(out_of_order.validate().is_err());

        let in_order = CreateArmature {
            bones: vec![parent, child],
            ..out_of_order
        };
        assert!(in_order.validate().is_ok());
    }

    #[test]
    fn mirror_names_round_trip() {
        assert_eq!(
            SideConvention::DotSuffix.mirror_name("Arm.L").as_deref(),
            Some("Arm.R")
        );
        assert_eq!(
            SideConvention::DotSuffix.mirror_name("Arm.R").as_deref(),
            Some("Arm.L")
        );
        assert_eq!(
            SideConvention::UnderscoreSuffix
                .mirror_name("Arm_L")
                .as_deref(),
            Some("Arm_R")
        );
        assert_eq!(
            SideConvention::WordPrefix
                .mirror_name("Left Arm")
                .as_deref(),
            Some("Right Arm")
        );
        assert_eq!(SideConvention::DotSuffix.mirror_name("Spine"), None);
    }

    #[test]
    fn ik_constraints_need_a_bone() {
        let params = ConstraintOperation {
            object: ObjectRef::name("Rig"),
            bone: None,
            constraint_type: ConstraintType::Ik,
            name: None,
            target: Some(ObjectRef::name("IK_Target")),
            subtarget: None,
            influence: None,
            chain_length: Some(2),
            properties: vec![],
        };
        assert!(params.validate().is_err());

        let params = ConstraintOperation {
            bone: Some("Forearm".into()),
            ..params
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn limit_constraints_need_no_target() {
        let params = ConstraintOperation {
            object: ObjectRef::name("Rig"),
            bone: Some("Head".into()),
            constraint_type: ConstraintType::LimitRotation,
            name: None,
            target: None,
            subtarget: None,
            influence: Some(1.0),
            chain_length: None,
            properties: vec![],
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn duplicate_bone_names_are_rejected() {
        let bone = BoneSpec {
            name: "Root".into(),
            head: Vec3::ZERO,
            tail: Vec3::Z,
            parent: None,
            connected: false,
            roll: 0.0,
            deform: true,
        };
        let params = CreateArmature {
            name: None,
            location: None,
            bones: vec![bone.clone(), bone],
            show_names: false,
            display_type: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn report_summarises_worst_severity() {
        let report = RigReport {
            findings: vec![
                RigFinding {
                    severity: crate::io::Severity::Info,
                    code: "A".into(),
                    entity: None,
                    message: String::new(),
                    suggested_fix: None,
                    details: Default::default(),
                },
                RigFinding {
                    severity: crate::io::Severity::Error,
                    code: "B".into(),
                    entity: None,
                    message: String::new(),
                    suggested_fix: None,
                    details: Default::default(),
                },
            ],
            ..Default::default()
        }
        .summarise();
        assert_eq!(report.worst_severity, Some(crate::io::Severity::Error));
    }
}

/// `rig.armature.get` and the diagnostics that take only an armature.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArmatureRefParams {
    pub armature: ArmatureRef,
}

/// `rig.bone.get` / `rig.bone.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BoneRefParams {
    pub armature: ArmatureRef,
    pub bone: String,
}

/// `rig.bone.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateBone {
    pub armature: ArmatureRef,
    #[serde(flatten)]
    pub bone: BoneSpec,
}

/// `rig.bone.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateBone {
    pub armature: ArmatureRef,
    /// Bone to change, by id or current name.
    pub bone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deform: Option<bool>,
}

/// `rig.vertex_group.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VertexGroupListParams {
    pub object: ObjectRef,
}

/// `rig.constraint.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListConstraints {
    pub object: ObjectRef,
    /// Pose bone whose constraints to list. Omit for object constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
}

/// `rig.constraint.update` / `rig.constraint.remove`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintRefParams {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    pub constraint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtarget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub influence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<crate::node_graph::PropertyAssignment>,
}

/// `rig.diagnostics.naming` / `rig.diagnostics.symmetry`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SymmetryDiagnostics {
    pub armature: ArmatureRef,
    /// Convention to assume. Detected from the bone names when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convention: Option<SideConvention>,
    /// Mirror axis, for symmetry checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<crate::math::Axis>,
    /// How far a pair may drift before it counts as asymmetric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
}

impl Validate for ArmatureRefParams {}
impl Validate for VertexGroupListParams {}
impl Validate for FixNaming {}
impl Validate for MirrorBones {}

impl Validate for BoneRefParams {
    fn validate(&self) -> Result<()> {
        check_name(&self.bone, "bone")
    }
}

impl Validate for CreateBone {
    fn validate(&self) -> Result<()> {
        self.bone.validate()
    }
}

impl Validate for UpdateBone {
    fn validate(&self) -> Result<()> {
        check_name(&self.bone, "bone")?;
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        self.head.check_finite("head")?;
        self.tail.check_finite("tail")?;
        if let Some(roll) = self.roll {
            crate::math::check_scalar_finite(roll, "roll")?;
        }
        if let (Some(head), Some(tail)) = (self.head, self.tail)
            && head.distance(tail) < 1e-6
        {
            return Err(BlenderError::invalid_argument(
                "Head and tail coincide, which would give the bone zero length.",
            ));
        }
        if self.name.is_none()
            && self.head.is_none()
            && self.tail.is_none()
            && self.roll.is_none()
            && self.deform.is_none()
        {
            return Err(BlenderError::invalid_argument(
                "Nothing to update on this bone.",
            ));
        }
        Ok(())
    }
}

impl Validate for ListConstraints {
    fn validate(&self) -> Result<()> {
        if let Some(bone) = &self.bone {
            check_name(bone, "bone")?;
        }
        Ok(())
    }
}

impl Validate for ConstraintRefParams {
    fn validate(&self) -> Result<()> {
        check_name(&self.constraint, "constraint")?;
        if let Some(bone) = &self.bone {
            check_name(bone, "bone")?;
        }
        if let Some(influence) = self.influence {
            check_range(influence, 0.0, 1.0, "influence")?;
        }
        for property in &self.properties {
            property.validate()?;
        }
        Ok(())
    }
}

impl Validate for SymmetryDiagnostics {
    fn validate(&self) -> Result<()> {
        if let Some(tolerance) = self.tolerance {
            check_non_negative(tolerance, "tolerance")?;
        }
        Ok(())
    }
}

impl Validate for ListBones {
    fn validate(&self) -> Result<()> {
        if let Some(parent) = &self.parent {
            check_name(parent, "parent")?;
        }
        self.page.validate()
    }
}

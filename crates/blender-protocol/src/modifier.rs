//! Modifier payloads.
//!
//! Modifiers are the widest, shallowest surface in Blender: two dozen types,
//! each with a dozen properties, most of them plain floats and enums. Giving
//! every property its own typed field would be thousands of lines that go stale
//! every release, so this module takes the middle road: the *type* is validated
//! against the connected build's capabilities, the common properties are typed,
//! and anything else goes through the same checked [`PropertyAssignment`]
//! mechanism the node graph uses -- name validated as an identifier, value
//! carrying its own type, and the bridge refusing to touch a property the
//! modifier does not advertise.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{ModifierId, ObjectRef},
    node_graph::PropertyAssignment,
};

/// Modifier types the bridge understands well enough to expose deliberately.
///
/// Availability is still checked against the connected build: a type listed
/// here that the running Blender does not register is reported as
/// `CAPABILITY_UNAVAILABLE`, not silently skipped.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModifierType {
    Array,
    Bevel,
    Boolean,
    Build,
    Cast,
    Curve,
    Decimate,
    Displace,
    EdgeSplit,
    Hook,
    Lattice,
    Mask,
    Mirror,
    Multires,
    Nodes,
    Remesh,
    Screw,
    Shrinkwrap,
    SimpleDeform,
    Skin,
    Smooth,
    Solidify,
    Subsurf,
    Triangulate,
    Weld,
    WeightedNormal,
    Wireframe,
}

impl ModifierType {
    pub const ALL: [ModifierType; 27] = [
        ModifierType::Array,
        ModifierType::Bevel,
        ModifierType::Boolean,
        ModifierType::Build,
        ModifierType::Cast,
        ModifierType::Curve,
        ModifierType::Decimate,
        ModifierType::Displace,
        ModifierType::EdgeSplit,
        ModifierType::Hook,
        ModifierType::Lattice,
        ModifierType::Mask,
        ModifierType::Mirror,
        ModifierType::Multires,
        ModifierType::Nodes,
        ModifierType::Remesh,
        ModifierType::Screw,
        ModifierType::Shrinkwrap,
        ModifierType::SimpleDeform,
        ModifierType::Skin,
        ModifierType::Smooth,
        ModifierType::Solidify,
        ModifierType::Subsurf,
        ModifierType::Triangulate,
        ModifierType::Weld,
        ModifierType::WeightedNormal,
        ModifierType::Wireframe,
    ];

    /// The identifier `object.modifier_add(type=...)` expects.
    pub fn blender_id(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Whether this modifier needs a target object to do anything.
    pub const fn requires_target(self) -> bool {
        matches!(
            self,
            ModifierType::Boolean
                | ModifierType::Curve
                | ModifierType::Hook
                | ModifierType::Lattice
                | ModifierType::Shrinkwrap
        )
    }
}

/// Common modifier properties, typed because nearly every call sets them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CommonModifierSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_viewport: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_render: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_in_editmode: Option<bool>,
    /// Object this modifier operates against, for the types that need one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ObjectRef>,
}

/// Anything the caller sent that no field claimed.
///
/// `serde(deny_unknown_fields)` is unavailable on these payloads -- it cannot
/// be combined with `flatten`, and `flatten` is what carries the common
/// settings. Without a catch-all the unmatched keys are simply dropped, which
/// is how a `settings: {...}` that should have been `properties: [...]` can
/// travel all the way to Blender and leave the modifier at its defaults: the
/// call reports success and nothing happened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct UnknownFields(
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub  std::collections::BTreeMap<String, serde_json::Value>,
);

impl UnknownFields {
    /// Refuse the call, naming what was not understood.
    fn check(&self, known: &[&str]) -> Result<()> {
        if let Some(name) = self.0.keys().next() {
            return Err(BlenderError::invalid_argument(format!(
                "`{name}` is not a field of this operation;                  type-specific settings go in `properties`"
            ))
            .with_detail("unexpected", name.clone())
            .with_detail("known_fields", known.join(", ")));
        }
        Ok(())
    }
}

/// `modifier.add`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddModifier {
    pub object: ObjectRef,
    #[serde(rename = "type")]
    pub modifier_type: ModifierType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, flatten)]
    pub common: CommonModifierSettings,
    /// Type-specific properties, e.g. `{"name": "levels", "value": {"int": 2}}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
    /// Insert at this position instead of appending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    #[serde(default, flatten)]
    pub unknown: UnknownFields,
}

impl Validate for AddModifier {
    fn validate(&self) -> Result<()> {
        self.unknown.check(&[
            "object",
            "type",
            "name",
            "properties",
            "index",
            "target",
            "show_viewport",
            "show_render",
            "show_in_editmode",
        ])?;
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if self.modifier_type.requires_target() && self.common.target.is_none() {
            return Err(BlenderError::invalid_argument(format!(
                "The {} modifier needs a `target` object to have any effect.",
                self.modifier_type.blender_id()
            ))
            .with_detail("field", "target")
            .with_detail("modifier_type", self.modifier_type.blender_id()));
        }
        for property in &self.properties {
            property.validate()?;
        }
        Ok(())
    }
}

/// `modifier.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateModifier {
    pub object: ObjectRef,
    /// Modifier name or stable id.
    pub modifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, flatten)]
    pub common: CommonModifierSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
    #[serde(default, flatten)]
    pub unknown: UnknownFields,
}

impl Validate for UpdateModifier {
    fn validate(&self) -> Result<()> {
        self.unknown.check(&[
            "object",
            "modifier",
            "name",
            "properties",
            "target",
            "show_viewport",
            "show_render",
            "show_in_editmode",
        ])?;
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        for property in &self.properties {
            property.validate()?;
        }
        if self.name.is_none()
            && self.properties.is_empty()
            && self.common.show_viewport.is_none()
            && self.common.show_render.is_none()
            && self.common.show_in_editmode.is_none()
            && self.common.target.is_none()
        {
            return Err(BlenderError::invalid_argument(
                "`modifier.update` needs at least one change.",
            ));
        }
        Ok(())
    }
}

/// Where `modifier.move` puts a modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MoveTarget {
    Up,
    Down,
    First,
    Last,
    /// Absolute zero-based position in the stack.
    Index(u32),
}

/// `modifier.move`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveModifier {
    pub object: ObjectRef,
    pub modifier: String,
    pub to: MoveTarget,
}

/// `modifier.apply`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplyModifier {
    pub object: ObjectRef,
    pub modifier: String,
    /// Apply to the object's shape keys as well, where Blender supports it.
    #[serde(default)]
    pub keep_shape_keys: bool,
    /// Apply every modifier at or before this one, in order.
    #[serde(default)]
    pub apply_preceding: bool,
}

/// `modifier.copy`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CopyModifiers {
    pub from: ObjectRef,
    pub to: Vec<ObjectRef>,
    /// Copy only these modifiers. Empty copies all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<String>,
    /// Remove the destination's existing modifiers first.
    #[serde(default)]
    pub replace: bool,
}

impl Validate for CopyModifiers {
    fn validate(&self) -> Result<()> {
        if self.to.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`to` must name at least one object.",
            ));
        }
        if self.to.contains(&self.from) {
            return Err(BlenderError::invalid_argument(
                "The source object is also a destination; that would be a no-op at best.",
            ));
        }
        Ok(())
    }
}

/// `modifier.list` filters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListModifiers {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_type: Option<ModifierType>,
    /// Include every property, not just the identifying ones.
    #[serde(default)]
    pub include_properties: bool,
    #[serde(default, flatten)]
    pub page: Page,
}

/// A modifier as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModifierDetail {
    pub id: ModifierId,
    pub name: String,
    #[serde(rename = "type")]
    pub modifier_type: String,
    pub index: u32,
    #[serde(default)]
    pub show_viewport: bool,
    #[serde(default)]
    pub show_render: bool,
    #[serde(default)]
    pub show_in_editmode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Populated when `include_properties` was set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
    /// True when the modifier is misconfigured, e.g. a boolean with no target.
    #[serde(default)]
    pub is_invalid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PropertyValue;

    #[test]
    fn boolean_modifier_requires_a_target() {
        let params = AddModifier {
            object: ObjectRef::name("Cube"),
            modifier_type: ModifierType::Boolean,
            name: None,
            common: CommonModifierSettings::default(),
            properties: vec![],
            index: None,
            unknown: UnknownFields::default(),
        };
        assert!(params.validate().is_err());

        let params = AddModifier {
            common: CommonModifierSettings {
                target: Some(ObjectRef::name("Cutter")),
                ..Default::default()
            },
            ..params
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn subsurf_needs_no_target() {
        let params = AddModifier {
            object: ObjectRef::name("Cube"),
            modifier_type: ModifierType::Subsurf,
            name: Some("Smooth".into()),
            common: CommonModifierSettings::default(),
            properties: vec![PropertyAssignment {
                name: "levels".into(),
                value: PropertyValue::Int(2),
            }],
            index: None,
            unknown: UnknownFields::default(),
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn blender_ids_are_screaming_snake() {
        assert_eq!(ModifierType::WeightedNormal.blender_id(), "WEIGHTED_NORMAL");
        assert_eq!(ModifierType::Subsurf.blender_id(), "SUBSURF");
    }

    #[test]
    fn copy_rejects_self_as_destination() {
        let params = CopyModifiers {
            from: ObjectRef::name("A"),
            to: vec![ObjectRef::name("A")],
            modifiers: vec![],
            replace: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn property_injection_is_rejected_at_the_modifier_layer() {
        let params = AddModifier {
            object: ObjectRef::name("Cube"),
            modifier_type: ModifierType::Subsurf,
            name: None,
            common: CommonModifierSettings::default(),
            properties: vec![PropertyAssignment {
                name: "levels)); import os; (".into(),
                value: PropertyValue::Int(2),
            }],
            index: None,
            unknown: UnknownFields::default(),
        };
        assert_eq!(
            params.validate().unwrap_err().code,
            crate::ErrorCode::InvalidProperty
        );
    }
}

/// `modifier.get` / `modifier.remove`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModifierRefParams {
    pub object: ObjectRef,
    /// Modifier name, or the stable id from `modifier.list`.
    pub modifier: String,
}

impl Validate for ModifierRefParams {}
impl Validate for MoveModifier {}

impl Validate for ApplyModifier {
    fn validate(&self) -> Result<()> {
        if self.modifier.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`modifier` must not be empty.",
            ));
        }
        Ok(())
    }
}

impl Validate for ListModifiers {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

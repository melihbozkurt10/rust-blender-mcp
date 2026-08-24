//! Collection payloads. Collections nest, so every operation guards against
//! creating a cycle before it reaches Blender.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{CollectionId, CollectionRef, ObjectRef},
};

/// `collection.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateCollection {
    pub name: String,
    /// Parent collection. Defaults to the scene's root collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CollectionRef>,
    /// Objects to link immediately, saving a round trip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    /// Colour tag (`COLOR_01` .. `COLOR_08`, or `NONE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_tag: Option<String>,
}

impl Validate for CreateCollection {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")?;
        if let Some(tag) = &self.color_tag {
            const TAGS: [&str; 9] = [
                "NONE", "COLOR_01", "COLOR_02", "COLOR_03", "COLOR_04", "COLOR_05", "COLOR_06",
                "COLOR_07", "COLOR_08",
            ];
            if !TAGS.contains(&tag.as_str()) {
                return Err(BlenderError::invalid_enum("color_tag", tag.clone(), TAGS));
            }
        }
        Ok(())
    }
}

/// `collection.link_object` / `collection.unlink_object` / `collection.move_object`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CollectionMembership {
    pub collection: CollectionRef,
    pub objects: Vec<ObjectRef>,
    /// For `move_object`: unlink from every other collection first. Ignored by
    /// link/unlink.
    #[serde(default)]
    pub exclusive: bool,
}

impl Validate for CollectionMembership {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object.",
            ));
        }
        Ok(())
    }
}

/// `collection.set_visibility`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CollectionVisibility {
    pub collection: CollectionRef,
    /// Hide in the viewport (the eye icon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_viewport: Option<bool>,
    /// Exclude from the view layer (the checkbox). Excluded collections are
    /// skipped by renders *and* by most operators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<bool>,
    /// Disable in renders (the camera icon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_render: Option<bool>,
    /// Make the collection unselectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_select: Option<bool>,
    /// Apply the same flags to nested collections.
    #[serde(default)]
    pub recursive: bool,
}

impl Validate for CollectionVisibility {
    fn validate(&self) -> Result<()> {
        if self.hide_viewport.is_none()
            && self.exclude.is_none()
            && self.hide_render.is_none()
            && self.hide_select.is_none()
        {
            return Err(BlenderError::invalid_argument(
                "Set at least one visibility flag.",
            ));
        }
        Ok(())
    }
}

/// `collection.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteCollection {
    pub collection: CollectionRef,
    /// Also delete the objects inside. When false (the default) objects are
    /// relinked to the parent collection so nothing silently disappears.
    #[serde(default)]
    pub delete_objects: bool,
    /// Recurse into child collections.
    #[serde(default)]
    pub recursive: bool,
}

/// `collection.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListCollections {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only direct children of this collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CollectionRef>,
    /// Include the full nested tree rather than one level.
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for ListCollections {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// A collection as reported by `collection.get` / `collection.list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CollectionSummary {
    pub id: CollectionId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CollectionId>,
    #[serde(default)]
    pub object_count: u32,
    #[serde(default)]
    pub child_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CollectionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<String>,
    #[serde(default)]
    pub hide_viewport: bool,
    #[serde(default)]
    pub hide_render: bool,
    #[serde(default)]
    pub exclude: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_colour_tags() {
        let params = CreateCollection {
            name: "Props".into(),
            parent: None,
            objects: vec![],
            color_tag: Some("COLOR_09".into()),
        };
        let err = params.validate().unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidEnum);
    }

    #[test]
    fn visibility_needs_at_least_one_flag() {
        let params = CollectionVisibility {
            collection: CollectionRef::name("Props"),
            hide_viewport: None,
            exclude: None,
            hide_render: None,
            hide_select: None,
            recursive: false,
        };
        assert!(params.validate().is_err());
    }
}

/// `collection.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetCollection {
    pub collection: CollectionRef,
}

/// `collection.rename`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameCollection {
    pub collection: CollectionRef,
    pub name: String,
}

impl Validate for GetCollection {}
impl Validate for DeleteCollection {}

impl Validate for RenameCollection {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")
    }
}

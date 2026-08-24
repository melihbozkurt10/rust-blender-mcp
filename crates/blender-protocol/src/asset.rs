//! External asset payloads.
//!
//! Licence metadata is carried through verbatim from the provider and is never
//! summarised into a "free to use" flag. Whether an asset may be used in a
//! given project is a legal question about a specific licence, and inventing a
//! reassuring boolean for it would be the most harmful thing this module could
//! do.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate,
    ids::{AssetId, CollectionRef},
    math::check_positive,
};

/// Asset kinds providers expose.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetType {
    Hdri,
    Texture,
    Model,
    Material,
}

/// `asset.search`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SearchAssets {
    /// Provider id, e.g. `polyhaven`. Omit to search every configured provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Free-text query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_type: Option<AssetType>,
    /// Provider category or tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Only assets under one of these licences, matched case-insensitively
    /// against the provider's licence identifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub licenses: Vec<String>,
    /// Minimum resolution in pixels on the longest edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_resolution: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_resolution: Option<u32>,
    /// Only assets available in this file format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Only assets that can be downloaded without an account.
    #[serde(default)]
    pub downloadable_only: bool,
    #[serde(default, flatten)]
    pub page: Page,
}

impl Validate for SearchAssets {
    fn validate(&self) -> Result<()> {
        self.page.validate()?;
        if let Some(query) = &self.query
            && query.len() > 200
        {
            return Err(BlenderError::invalid_argument("`query` is too long."));
        }
        if let (Some(min), Some(max)) = (self.min_resolution, self.max_resolution)
            && min > max
        {
            return Err(BlenderError::invalid_argument(format!(
                "`min_resolution` ({min}) exceeds `max_resolution` ({max})."
            )));
        }
        if self.query.is_none()
            && self.asset_type.is_none()
            && self.categories.is_empty()
            && self.provider.is_none()
        {
            return Err(BlenderError::invalid_argument(
                "Give at least a `query`, `asset_type`, `categories` or `provider`; an unfiltered search across every provider is not useful.",
            ));
        }
        Ok(())
    }
}

/// Licence information, passed through from the provider unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct License {
    /// The provider's own identifier, e.g. `CC0`, `CC-BY-4.0`, `standard`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether the provider states attribution is required. `None` means the
    /// provider did not say -- which is not the same as "no".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_attribution: Option<bool>,
    /// Whether the provider states commercial use is permitted. `None` means
    /// unstated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commercial_use: Option<bool>,
}

/// An asset as returned by a provider search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssetSummary {
    pub id: AssetId,
    /// The provider's own identifier for this asset.
    pub provider_id: String,
    pub provider: String,
    pub title: String,
    pub asset_type: AssetType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    /// Page a human can open to see the asset in context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Downloadable variants: resolution and format combinations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<AssetVariant>,
    /// Whether downloading needs credentials the server does not have.
    #[serde(default)]
    pub requires_auth: bool,
}

/// One downloadable form of an asset.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssetVariant {
    /// Provider's label, e.g. `4k`, `2k`, `gltf`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Which map this file holds, for texture sets: `diffuse`, `normal`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
}

/// `asset.download`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownloadAsset {
    pub provider: String,
    /// The provider's asset identifier, from a search result.
    pub asset_id: String,
    /// Variant to fetch. Defaults to the provider's recommended one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Preferred resolution, when the provider offers several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// For texture sets: which maps to fetch. Empty fetches the standard set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maps: Vec<String>,
    /// Re-download even if a cached copy exists.
    #[serde(default)]
    pub force: bool,
}

impl Validate for DownloadAsset {
    fn validate(&self) -> Result<()> {
        check_provider_id(&self.provider)?;
        check_asset_id(&self.asset_id)?;
        if let Some(resolution) = self.resolution {
            check_positive(resolution as f64, "resolution")?;
            if resolution > 16384 {
                return Err(BlenderError::invalid_argument(
                    "`resolution` above 16384 is beyond anything a provider offers.",
                ));
            }
        }
        Ok(())
    }
}

/// `asset.import`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportAsset {
    #[serde(flatten)]
    pub download: DownloadAsset,
    /// Collection to place imported objects into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    /// For HDRIs: set them as the world environment rather than just loading
    /// the image.
    #[serde(default = "crate::object::default_true")]
    pub apply_as_world: bool,
    /// For textures: build a PBR material from the downloaded maps.
    #[serde(default = "crate::object::default_true")]
    pub build_material: bool,
    /// Name for the created material or object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Validate for ImportAsset {
    fn validate(&self) -> Result<()> {
        self.download.validate()?;
        if let Some(name) = &self.name {
            crate::check_name(name, "name")?;
        }
        Ok(())
    }
}

/// A completed download.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownloadedAsset {
    pub asset: AssetSummary,
    /// Files that landed in the managed downloads root.
    pub files: Vec<DownloadedFile>,
    /// Whether this came from the local cache rather than the network.
    #[serde(default)]
    pub from_cache: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// One downloaded file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownloadedFile {
    /// Path relative to the downloads root.
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A configured provider.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub asset_types: Vec<AssetType>,
    /// Whether credentials are configured for this provider.
    #[serde(default)]
    pub authenticated: bool,
    /// Whether the provider needs credentials for any operation at all.
    #[serde(default)]
    pub requires_auth: bool,
    /// Search filters this provider honours.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_filters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_summary: Option<String>,
}

/// Provider ids are short lowercase identifiers, and end up in filesystem
/// paths, so they are constrained tightly.
pub fn check_provider_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 32 {
        return Err(BlenderError::invalid_argument(
            "`provider` must be between 1 and 32 characters.",
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(BlenderError::invalid_argument(format!(
            "`{id}` is not a valid provider id; use lowercase letters, digits and underscores."
        ))
        .with_detail("provider", id));
    }
    Ok(())
}

/// Asset ids come from provider APIs and are used to build cache paths, so
/// separators and traversal sequences are rejected outright.
pub fn check_asset_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 200 {
        return Err(BlenderError::invalid_argument(
            "`asset_id` must be between 1 and 200 characters.",
        ));
    }
    if id.contains(['/', '\\', '\0']) || id.contains("..") {
        return Err(BlenderError::new(
            crate::ErrorCode::InvalidArgument,
            "`asset_id` must not contain path separators or `..`.",
        )
        .with_detail("asset_id", id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_constrained() {
        assert!(check_provider_id("polyhaven").is_ok());
        assert!(check_provider_id("poly_haven2").is_ok());
        assert!(check_provider_id("Poly Haven").is_err());
        assert!(check_provider_id("../etc").is_err());
        assert!(check_provider_id("").is_err());
    }

    #[test]
    fn asset_ids_cannot_walk_the_filesystem() {
        assert!(check_asset_id("rocky_terrain_02").is_ok());
        assert!(check_asset_id("../../../../etc/passwd").is_err());
        assert!(check_asset_id("a\\b").is_err());
    }

    #[test]
    fn unfiltered_searches_are_refused() {
        assert!(SearchAssets::default().validate().is_err());
        let params = SearchAssets {
            query: Some("concrete".into()),
            ..Default::default()
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn inverted_resolution_bounds_are_rejected() {
        let params = SearchAssets {
            query: Some("wood".into()),
            min_resolution: Some(4096),
            max_resolution: Some(1024),
            ..Default::default()
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn license_fields_stay_tri_state() {
        // An unstated licence term must serialise as absent, never as `false`,
        // so a caller cannot read "not stated" as "not allowed" or vice versa.
        let license = License {
            id: "standard".into(),
            name: None,
            url: None,
            requires_attribution: None,
            commercial_use: None,
        };
        let json = serde_json::to_value(&license).unwrap();
        assert!(json.get("commercial_use").is_none(), "got {json}");
    }
}

//! The provider trait and the shapes it deals in.

use std::{future::Future, pin::Pin};

use blender_protocol::{
    Result,
    asset::{AssetSummary, DownloadAsset, ProviderInfo, SearchAssets},
    ids::AssetId,
};
use uuid::Uuid;

use crate::http::Authorization;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One file a download will fetch.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub url: String,
    /// Name to write it under, inside the asset's cache directory.
    pub filename: String,
    /// Which texture map this file holds, if it is one.
    pub map: Option<String>,
    /// Size the provider claims, used to refuse an oversized download before
    /// starting it.
    pub size_bytes: Option<u64>,
    /// Whether this URL needs the provider's credentials. Signed CDN URLs do
    /// not, and sending a token to a CDN would leak it.
    pub authenticated: bool,
}

impl PlannedFile {
    pub fn new(url: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            filename: filename.into(),
            map: None,
            size_bytes: None,
            authenticated: false,
        }
    }

    pub fn with_map(mut self, map: impl Into<String>) -> Self {
        self.map = Some(map.into());
        self
    }

    pub fn with_size(mut self, size: Option<u64>) -> Self {
        self.size_bytes = size;
        self
    }
}

/// What a provider decided a download request means.
#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub asset: AssetSummary,
    /// The variant actually chosen, which may not be the one asked for: a
    /// request for 8k against an asset that stops at 4k resolves here, once,
    /// rather than failing.
    pub variant: String,
    pub files: Vec<PlannedFile>,
}

/// An external asset library.
///
/// Object-safe on purpose: providers are held as `Arc<dyn AssetProvider>` so
/// the set can be configured at runtime, and so a test can substitute one.
pub trait AssetProvider: Send + Sync {
    /// Short lowercase id, used in tool arguments and cache paths.
    fn id(&self) -> &'static str;

    /// What this provider is and what it can do, for `asset.providers`.
    fn info(&self) -> ProviderInfo;

    /// Credentials for API calls, if this provider has any configured.
    fn authorization(&self) -> Option<Authorization> {
        None
    }

    fn search<'a>(&'a self, query: &'a SearchAssets) -> BoxFuture<'a, Result<Vec<AssetSummary>>>;

    fn get<'a>(&'a self, asset_id: &'a str) -> BoxFuture<'a, Result<AssetSummary>>;

    /// Work out which files a download request means, without fetching any.
    fn plan<'a>(&'a self, request: &'a DownloadAsset) -> BoxFuture<'a, Result<DownloadPlan>>;
}

/// The namespace for asset ids.
///
/// A fixed random UUID. Nothing depends on its value beyond being stable.
const ASSET_NAMESPACE: Uuid = Uuid::from_u128(0x6f2a_1c84_9b3d_4e51_a7c6_08d5_23bf_7e90);

/// A stable id for an external asset.
///
/// Derived from the provider and its own identifier rather than generated, so
/// the same asset carries the same id across searches, across restarts and
/// across machines. A random id would make an id in a conversation useless the
/// moment the server restarted.
pub fn asset_id(provider: &str, provider_asset_id: &str) -> AssetId {
    // Length-prefixed rather than separated: with a separator, an asset id that
    // happened to contain one could collide with a different provider's asset.
    let name = format!("{}:{provider}{provider_asset_id}", provider.len());
    AssetId::from_uuid(Uuid::new_v5(&ASSET_NAMESPACE, name.as_bytes()))
}

/// Pick a resolution from the ladder a provider publishes.
///
/// Providers offer fixed rungs (`1k`, `2k`, `4k`, `8k`). A request between two
/// of them takes the rung above rather than the nearer one: asking for 3000 and
/// silently getting 2048 is a quality regression the caller cannot see, while
/// getting 4096 is only a larger file. A request for more than exists takes the
/// largest, because refusing would be unhelpful when the answer is unambiguous.
pub fn nearest_resolution(available: &[u32], wanted: Option<u32>) -> Option<u32> {
    let wanted = match wanted {
        Some(wanted) => wanted,
        None => return preferred_default(available),
    };
    available
        .iter()
        .copied()
        .filter(|candidate| *candidate >= wanted)
        .min()
        .or_else(|| available.iter().copied().max())
}

/// The default when no resolution was asked for: 2k if offered, else the
/// largest at or below 4k, else the smallest. Downloading an 8k texture set
/// because nobody said otherwise wastes bandwidth and disk.
fn preferred_default(available: &[u32]) -> Option<u32> {
    if available.contains(&2048) {
        return Some(2048);
    }
    available
        .iter()
        .copied()
        .filter(|candidate| *candidate <= 4096)
        .max()
        .or_else(|| available.iter().copied().min())
}

/// Turn `4k` into `4096`, and anything else into `None`.
pub fn parse_resolution_label(label: &str) -> Option<u32> {
    let label = label.trim().to_ascii_lowercase();
    let digits = label.strip_suffix('k')?;
    let value: u32 = digits.parse().ok()?;
    value.checked_mul(1024)
}

/// The inverse: `4096` becomes `4k`.
pub fn resolution_label(pixels: u32) -> String {
    if pixels.is_multiple_of(1024) {
        format!("{}k", pixels / 1024)
    } else {
        pixels.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_ids_are_stable_and_provider_scoped() {
        let first = asset_id("polyhaven", "rocky_terrain_02");
        assert_eq!(first, asset_id("polyhaven", "rocky_terrain_02"));
        assert_ne!(first, asset_id("sketchfab", "rocky_terrain_02"));
        // A boundary an asset id could otherwise forge.
        assert_ne!(asset_id("ab", "c"), asset_id("a", "bc"));
    }

    #[test]
    fn resolution_labels_round_trip() {
        assert_eq!(parse_resolution_label("4k"), Some(4096));
        assert_eq!(parse_resolution_label("16K"), Some(16384));
        assert_eq!(parse_resolution_label("blend"), None);
        assert_eq!(resolution_label(2048), "2k");
        assert_eq!(resolution_label(1500), "1500");
    }

    #[test]
    fn the_nearest_rung_is_chosen_rather_than_failing() {
        let ladder = [1024, 2048, 4096, 8192];
        assert_eq!(nearest_resolution(&ladder, Some(3000)), Some(4096));
        assert_eq!(nearest_resolution(&ladder, Some(16384)), Some(8192));
        assert_eq!(nearest_resolution(&ladder, Some(1)), Some(1024));
        assert_eq!(nearest_resolution(&[], Some(1024)), None);
    }

    #[test]
    fn the_default_resolution_is_modest() {
        assert_eq!(nearest_resolution(&[1024, 2048, 8192], None), Some(2048));
        assert_eq!(
            nearest_resolution(&[1024, 4096, 8192], None),
            Some(4096),
            "without 2k, the largest sensible rung"
        );
        assert_eq!(
            nearest_resolution(&[8192, 16384], None),
            Some(8192),
            "when everything is huge, the smallest"
        );
    }
}

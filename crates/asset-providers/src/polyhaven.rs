//! Poly Haven.
//!
//! A public API with no credentials: <https://api.polyhaven.com>. Every asset
//! on Poly Haven is published under CC0, which the provider states site-wide;
//! that statement is passed through as licence metadata rather than reduced to
//! a "free to use" flag.

use std::{collections::BTreeMap, sync::Arc};

use blender_protocol::{
    BlenderError, ErrorCode, Result,
    asset::{
        AssetSummary, AssetType, AssetVariant, DownloadAsset, License, ProviderInfo, SearchAssets,
    },
};
use serde_json::Value;

use crate::{
    http::Fetcher,
    provider::{
        AssetProvider, BoxFuture, DownloadPlan, PlannedFile, asset_id, nearest_resolution,
        parse_resolution_label, resolution_label,
    },
};

pub const ID: &str = "polyhaven";
const API: &str = "https://api.polyhaven.com";
const CDN_THUMBNAIL: &str = "https://cdn.polyhaven.com/asset_img/thumbs";

/// Top-level keys in a `/files/<id>` response that are whole-asset formats
/// rather than texture maps.
const FORMAT_KEYS: &[&str] = &["blend", "gltf", "fbx", "usd", "mtlx"];

/// The maps a PBR material actually needs, in the order they are looked for.
/// Fetching every map a texture offers would triple the download for maps most
/// materials never wire up.
const DEFAULT_MAPS: &[&str] = &["Diffuse", "nor_gl", "Rough", "Displacement", "AO", "Metal"];

pub struct PolyHaven {
    fetcher: Arc<dyn Fetcher>,
}

impl PolyHaven {
    pub fn new(fetcher: Arc<dyn Fetcher>) -> Self {
        Self { fetcher }
    }

    async fn files(&self, asset: &str) -> Result<Value> {
        self.fetcher
            .get_json(&format!("{API}/files/{asset}"), None)
            .await
    }

    async fn asset_info(&self, asset: &str) -> Result<Value> {
        self.fetcher
            .get_json(&format!("{API}/info/{asset}"), None)
            .await
    }
}

/// Poly Haven's CC0 statement, carried through verbatim.
fn cc0() -> License {
    License {
        id: "CC0".into(),
        name: Some("CC0 1.0 Universal (public domain dedication)".into()),
        url: Some("https://creativecommons.org/publicdomain/zero/1.0/".into()),
        requires_attribution: Some(false),
        commercial_use: Some(true),
    }
}

fn asset_type_from_code(code: i64) -> Option<AssetType> {
    match code {
        0 => Some(AssetType::Hdri),
        1 => Some(AssetType::Texture),
        2 => Some(AssetType::Model),
        _ => None,
    }
}

fn type_query(asset_type: Option<AssetType>) -> Option<&'static str> {
    match asset_type {
        Some(AssetType::Hdri) => Some("hdris"),
        Some(AssetType::Texture) => Some("textures"),
        Some(AssetType::Model) => Some("models"),
        // Poly Haven has no separate material category; its textures are the
        // material assets.
        Some(AssetType::Material) => Some("textures"),
        None => None,
    }
}

/// Turn one entry of the `/assets` map into a summary.
pub(crate) fn summary_from_entry(id: &str, entry: &Value) -> Option<AssetSummary> {
    let asset_type = asset_type_from_code(entry.get("type")?.as_i64()?)?;
    let title = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();

    let authors = entry
        .get("authors")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect::<Vec<_>>());

    let strings = |key: &str| -> Vec<String> {
        entry
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    Some(AssetSummary {
        id: asset_id(ID, id),
        provider_id: id.to_string(),
        provider: ID.to_string(),
        title,
        asset_type,
        authors,
        source_url: Some(format!("https://polyhaven.com/a/{id}")),
        thumbnail_url: Some(format!("{CDN_THUMBNAIL}/{id}.png?width=256&height=180")),
        license: Some(cc0()),
        categories: strings("categories"),
        tags: strings("tags"),
        variants: vec![],
        requires_auth: false,
    })
}

/// Whether a summary matches the free-text and category filters.
///
/// Poly Haven's `/assets` endpoint has no search parameter, so filtering is
/// done here over the full list. That is the provider's design, not a shortcut:
/// the list is a few thousand entries and is the only thing it will return.
pub(crate) fn matches(summary: &AssetSummary, query: &SearchAssets) -> bool {
    if let Some(text) = &query.query {
        let needle = text.trim().to_lowercase();
        let haystack_hit = summary.title.to_lowercase().contains(&needle)
            || summary.provider_id.to_lowercase().contains(&needle)
            || summary
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(&needle))
            || summary
                .categories
                .iter()
                .any(|category| category.to_lowercase().contains(&needle));
        if !haystack_hit {
            return false;
        }
    }
    if !query.categories.is_empty() {
        let wanted: Vec<String> = query
            .categories
            .iter()
            .map(|category| category.to_lowercase())
            .collect();
        let has = summary
            .categories
            .iter()
            .any(|category| wanted.contains(&category.to_lowercase()))
            || summary
                .tags
                .iter()
                .any(|tag| wanted.contains(&tag.to_lowercase()));
        if !has {
            return false;
        }
    }
    if !query.licenses.is_empty() {
        let matched = summary.license.as_ref().is_some_and(|license| {
            query
                .licenses
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(&license.id))
        });
        if !matched {
            return false;
        }
    }
    true
}

/// The resolutions offered for a `/files` sub-tree keyed by resolution label.
fn resolutions(node: &Value) -> Vec<u32> {
    let mut found: Vec<u32> = node
        .as_object()
        .map(|map| {
            map.keys()
                .filter_map(|key| parse_resolution_label(key))
                .collect()
        })
        .unwrap_or_default();
    found.sort_unstable();
    found.dedup();
    found
}

/// The formats offered at one resolution.
fn formats(node: &Value) -> Vec<String> {
    node.as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn pick_format(available: &[String], wanted: Option<&str>, preference: &[&str]) -> Option<String> {
    if let Some(wanted) = wanted {
        return available
            .iter()
            .find(|format| format.eq_ignore_ascii_case(wanted))
            .cloned();
    }
    preference
        .iter()
        .find_map(|preferred| {
            available
                .iter()
                .find(|format| format.eq_ignore_ascii_case(preferred))
                .cloned()
        })
        .or_else(|| available.first().cloned())
}

fn file_entry(node: &Value) -> Option<(String, Option<u64>)> {
    let url = node.get("url")?.as_str()?.to_string();
    let size = node.get("size").and_then(Value::as_u64);
    Some((url, size))
}

/// The texture maps a request asks for, or the standard set.
fn wanted_maps(available: &[String], requested: &[String]) -> Vec<String> {
    if !requested.is_empty() {
        return available
            .iter()
            .filter(|name| {
                requested
                    .iter()
                    .any(|wanted| wanted.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
    }
    let mut chosen: Vec<String> = DEFAULT_MAPS
        .iter()
        .filter_map(|preferred| {
            available
                .iter()
                .find(|name| name.eq_ignore_ascii_case(preferred))
                .cloned()
        })
        .collect();
    if chosen.is_empty() {
        // An unfamiliar texture set is better served whole than not at all.
        chosen = available.to_vec();
    }
    chosen
}

/// Build the file list for a download.
///
/// Split out from the trait method so it can be tested against a recorded
/// `/files` response without a fetcher at all.
pub(crate) fn plan_files(
    asset: &AssetSummary,
    files: &Value,
    request: &DownloadAsset,
) -> Result<(String, Vec<PlannedFile>)> {
    let root = files.as_object().ok_or_else(|| {
        BlenderError::new(
            ErrorCode::AssetProviderError,
            "Poly Haven returned a file listing that is not an object.",
        )
    })?;

    match asset.asset_type {
        AssetType::Hdri => {
            let node = root.get("hdri").ok_or_else(|| missing("hdri"))?;
            let available = resolutions(node);
            let resolution = nearest_resolution(&available, requested_resolution(request))
                .ok_or_else(|| missing("any resolution"))?;
            let label = resolution_label(resolution);
            let at_resolution = node.get(&label).ok_or_else(|| missing(&label))?;
            let format = pick_format(
                &formats(at_resolution),
                request.format.as_deref(),
                &["hdr", "exr"],
            )
            .ok_or_else(|| missing("a usable format"))?;
            let (url, size) =
                file_entry(at_resolution.get(&format).ok_or_else(|| missing(&format))?)
                    .ok_or_else(|| missing("a download URL"))?;

            let filename = format!("{}_{label}.{format}", asset.provider_id);
            Ok((label, vec![PlannedFile::new(url, filename).with_size(size)]))
        }
        AssetType::Texture | AssetType::Material => {
            let map_names: Vec<String> = root
                .keys()
                .filter(|key| !FORMAT_KEYS.contains(&key.as_str()))
                .cloned()
                .collect();
            let chosen = wanted_maps(&map_names, &request.maps);
            if chosen.is_empty() {
                return Err(BlenderError::new(
                    ErrorCode::AssetNotFound,
                    format!(
                        "None of the requested maps exist for `{}`.",
                        asset.provider_id
                    ),
                )
                .with_detail_json("available_maps", &map_names));
            }

            // One resolution for the whole set: mixed resolutions in a single
            // material is a rendering bug waiting to happen.
            let available = chosen
                .iter()
                .filter_map(|name| root.get(name))
                .flat_map(resolutions)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let resolution = nearest_resolution(&available, requested_resolution(request))
                .ok_or_else(|| missing("any resolution"))?;
            let label = resolution_label(resolution);

            let mut planned = Vec::new();
            for name in chosen {
                let Some(at_resolution) = root.get(&name).and_then(|node| node.get(&label)) else {
                    continue;
                };
                let Some(format) = pick_format(
                    &formats(at_resolution),
                    request.format.as_deref(),
                    &["jpg", "png", "exr", "tif"],
                ) else {
                    continue;
                };
                let Some((url, size)) = at_resolution.get(&format).and_then(file_entry) else {
                    continue;
                };
                let filename = format!("{}_{name}_{label}.{format}", asset.provider_id);
                planned.push(
                    PlannedFile::new(url, filename)
                        .with_map(name)
                        .with_size(size),
                );
            }

            if planned.is_empty() {
                return Err(missing(&format!("any file at {label}")));
            }
            Ok((label, planned))
        }
        AssetType::Model => {
            // A model listing has its texture maps at the top level beside its
            // formats -- `cart_diff`, `props_arm` and so on -- so the
            // candidates are narrowed to the keys that really are formats.
            // Otherwise a `format` of `cart_diff` would be accepted and produce
            // a "model" that is one PNG.
            let candidates: Vec<String> = root
                .keys()
                .filter(|key| FORMAT_KEYS.contains(&key.as_str()))
                .cloned()
                .collect();
            let format = pick_format(
                &candidates,
                request.format.as_deref(),
                &["blend", "gltf", "fbx", "usd"],
            )
            .ok_or_else(|| missing("a model format"))?;
            let node = root.get(&format).ok_or_else(|| missing(&format))?;
            let available = resolutions(node);
            let resolution = nearest_resolution(&available, requested_resolution(request))
                .ok_or_else(|| missing("any resolution"))?;
            let label = resolution_label(resolution);
            let at_resolution = node.get(&label).ok_or_else(|| missing(&label))?;

            let inner = pick_format(&formats(at_resolution), Some(&format), &[])
                .or_else(|| formats(at_resolution).first().cloned())
                .ok_or_else(|| missing("a model file"))?;
            let entry = at_resolution.get(&inner).ok_or_else(|| missing(&inner))?;
            let (url, size) = file_entry(entry).ok_or_else(|| missing("a download URL"))?;

            let mut planned = vec![
                PlannedFile::new(url, format!("{}_{label}.{inner}", asset.provider_id))
                    .with_size(size),
            ];

            // A .blend or .gltf refers to its textures by relative path; those
            // paths are preserved so the model opens with its maps attached.
            if let Some(includes) = entry.get("include").and_then(Value::as_object) {
                for (path, include) in includes {
                    if let Some((url, size)) = file_entry(include) {
                        planned.push(PlannedFile::new(url, path.clone()).with_size(size));
                    }
                }
            }

            Ok((label, planned))
        }
    }
}

fn requested_resolution(request: &DownloadAsset) -> Option<u32> {
    request
        .resolution
        .or_else(|| request.variant.as_deref().and_then(parse_resolution_label))
}

fn missing(what: &str) -> BlenderError {
    BlenderError::new(
        ErrorCode::AssetNotFound,
        format!("Poly Haven does not offer {what} for this asset."),
    )
}

/// Describe what can be downloaded, for a search result.
fn variants_from_files(files: &Value, asset_type: AssetType) -> Vec<AssetVariant> {
    let Some(root) = files.as_object() else {
        return vec![];
    };
    let node = match asset_type {
        AssetType::Hdri => root.get("hdri"),
        AssetType::Model => FORMAT_KEYS.iter().find_map(|key| root.get(*key)),
        AssetType::Texture | AssetType::Material => root
            .iter()
            .find(|(key, _)| !FORMAT_KEYS.contains(&key.as_str()))
            .map(|(_, value)| value),
    };
    let Some(node) = node else {
        return vec![];
    };

    // Keyed by pixel count, not by label: sorting `1k`, `2k`, `16k`, `24k` as
    // strings puts 16k before 1k, which reads as a listing bug to anyone
    // choosing a resolution from it.
    let mut by_resolution: BTreeMap<u32, AssetVariant> = BTreeMap::new();
    for resolution in resolutions(node) {
        let label = resolution_label(resolution);
        let format = node
            .get(&label)
            .map(formats)
            .and_then(|formats| formats.first().cloned());
        by_resolution.insert(
            resolution,
            AssetVariant {
                id: label,
                resolution: Some(resolution),
                format,
                size_bytes: None,
                map: None,
            },
        );
    }
    by_resolution.into_values().collect()
}

impl AssetProvider for PolyHaven {
    fn id(&self) -> &'static str {
        ID
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            name: "Poly Haven".into(),
            url: Some("https://polyhaven.com".into()),
            asset_types: vec![AssetType::Hdri, AssetType::Texture, AssetType::Model],
            authenticated: false,
            requires_auth: false,
            supported_filters: vec![
                "query".into(),
                "asset_type".into(),
                "categories".into(),
                "licenses".into(),
                "format".into(),
            ],
            license_summary: Some(
                "Poly Haven publishes every asset under CC0. Attribution is not required, but \
                 confirm the licence on the asset page before relying on it."
                    .into(),
            ),
        }
    }

    fn search<'a>(&'a self, query: &'a SearchAssets) -> BoxFuture<'a, Result<Vec<AssetSummary>>> {
        Box::pin(async move {
            let url = match type_query(query.asset_type) {
                Some(kind) => format!("{API}/assets?t={kind}"),
                None => format!("{API}/assets"),
            };
            let listing = self.fetcher.get_json(&url, None).await?;
            let entries = listing.as_object().ok_or_else(|| {
                BlenderError::new(
                    ErrorCode::AssetProviderError,
                    "Poly Haven returned an asset list that is not an object.",
                )
            })?;

            let mut summaries: Vec<AssetSummary> = entries
                .iter()
                .filter_map(|(id, entry)| summary_from_entry(id, entry))
                .filter(|summary| matches(summary, query))
                .collect();
            // The API's map order is not stable enough to page over, so results
            // are sorted by title; a cursor into an unstable order would skip
            // and repeat entries between pages.
            summaries.sort_by(|a, b| {
                a.title
                    .cmp(&b.title)
                    .then(a.provider_id.cmp(&b.provider_id))
            });
            Ok(summaries)
        })
    }

    fn get<'a>(&'a self, provider_asset_id: &'a str) -> BoxFuture<'a, Result<AssetSummary>> {
        Box::pin(async move {
            let info = self.asset_info(provider_asset_id).await?;
            let mut summary = summary_from_entry(provider_asset_id, &info).ok_or_else(|| {
                BlenderError::new(
                    ErrorCode::AssetNotFound,
                    format!("Poly Haven has no asset called `{provider_asset_id}`."),
                )
                .with_detail("asset_id", provider_asset_id)
            })?;
            if let Ok(files) = self.files(provider_asset_id).await {
                summary.variants = variants_from_files(&files, summary.asset_type);
            }
            Ok(summary)
        })
    }

    fn plan<'a>(&'a self, request: &'a DownloadAsset) -> BoxFuture<'a, Result<DownloadPlan>> {
        Box::pin(async move {
            let asset = self.get(&request.asset_id).await?;
            let files = self.files(&request.asset_id).await?;
            let (variant, planned) = plan_files(&asset, &files, request)?;
            Ok(DownloadPlan {
                asset,
                variant,
                files: planned,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::http::stub::StubFetcher;

    fn hdri_files() -> Value {
        json!({
            "hdri": {
                "1k": {"hdr": {"url": "https://dl.polyhaven.org/a_1k.hdr", "size": 1000}},
                "4k": {
                    "hdr": {"url": "https://dl.polyhaven.org/a_4k.hdr", "size": 4000},
                    "exr": {"url": "https://dl.polyhaven.org/a_4k.exr", "size": 9000}
                }
            }
        })
    }

    fn texture_files() -> Value {
        json!({
            "blend": {"1k": {"blend": {"url": "https://dl.polyhaven.org/t.blend"}}},
            "Diffuse": {
                "1k": {"jpg": {"url": "https://dl.polyhaven.org/d_1k.jpg", "size": 100}},
                "2k": {"jpg": {"url": "https://dl.polyhaven.org/d_2k.jpg", "size": 200}}
            },
            "nor_gl": {
                "2k": {"exr": {"url": "https://dl.polyhaven.org/n_2k.exr", "size": 300}}
            },
            "Rough": {
                "2k": {"jpg": {"url": "https://dl.polyhaven.org/r_2k.jpg", "size": 150}}
            },
            "Curiosity": {
                "2k": {"jpg": {"url": "https://dl.polyhaven.org/c_2k.jpg", "size": 50}}
            }
        })
    }

    fn summary(kind: AssetType) -> AssetSummary {
        AssetSummary {
            id: asset_id(ID, "rocky"),
            provider_id: "rocky".into(),
            provider: ID.into(),
            title: "Rocky".into(),
            asset_type: kind,
            authors: None,
            source_url: None,
            thumbnail_url: None,
            license: Some(cc0()),
            categories: vec![],
            tags: vec![],
            variants: vec![],
            requires_auth: false,
        }
    }

    fn request() -> DownloadAsset {
        DownloadAsset {
            provider: ID.into(),
            asset_id: "rocky".into(),
            variant: None,
            resolution: None,
            format: None,
            maps: vec![],
            force: false,
        }
    }

    #[test]
    fn an_hdri_download_picks_the_nearest_resolution_and_a_sane_format() {
        let (variant, files) =
            plan_files(&summary(AssetType::Hdri), &hdri_files(), &request()).unwrap();
        assert_eq!(
            variant, "4k",
            "with no 2k rung, the default is the largest up to 4k"
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "rocky_4k.hdr");
        assert_eq!(files[0].size_bytes, Some(4000));

        let mut asked = request();
        asked.resolution = Some(4096);
        asked.format = Some("exr".into());
        let (variant, files) =
            plan_files(&summary(AssetType::Hdri), &hdri_files(), &asked).unwrap();
        assert_eq!(variant, "4k");
        assert_eq!(files[0].filename, "rocky_4k.exr");
    }

    #[test]
    fn a_variant_label_works_as_well_as_a_resolution() {
        let mut asked = request();
        asked.variant = Some("4k".into());
        let (variant, _) = plan_files(&summary(AssetType::Hdri), &hdri_files(), &asked).unwrap();
        assert_eq!(variant, "4k");
    }

    #[test]
    fn a_texture_set_fetches_the_standard_maps_at_one_resolution() {
        let (variant, files) =
            plan_files(&summary(AssetType::Texture), &texture_files(), &request()).unwrap();
        assert_eq!(variant, "2k");

        let maps: Vec<&str> = files
            .iter()
            .filter_map(|file| file.map.as_deref())
            .collect();
        assert_eq!(maps, vec!["Diffuse", "nor_gl", "Rough"]);
        assert!(
            !maps.contains(&"Curiosity"),
            "an unrecognised map is not fetched by default"
        );
        assert!(
            files.iter().all(|file| file.filename.contains("_2k.")),
            "a material must not mix resolutions: {files:?}"
        );
    }

    #[test]
    fn specific_maps_can_be_asked_for() {
        let mut asked = request();
        asked.maps = vec!["curiosity".into(), "rough".into()];
        let (_, files) =
            plan_files(&summary(AssetType::Texture), &texture_files(), &asked).unwrap();
        let maps: Vec<&str> = files.iter().filter_map(|f| f.map.as_deref()).collect();
        assert_eq!(
            maps,
            vec!["Rough", "Curiosity"],
            "matching is case-insensitive, and the provider's own order is kept"
        );
    }

    #[test]
    fn a_map_that_does_not_exist_is_reported_with_the_ones_that_do() {
        let mut asked = request();
        asked.maps = vec!["Subsurface".into()];
        let error = plan_files(&summary(AssetType::Texture), &texture_files(), &asked).unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetNotFound);
        assert!(
            error.details["available_maps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "Diffuse"),
            "the caller is told what it could have asked for"
        );
    }

    #[test]
    fn a_model_brings_its_textures_with_it() {
        let files = json!({
            "blend": {
                "2k": {
                    "blend": {
                        "url": "https://dl.polyhaven.org/m.blend",
                        "size": 5000,
                        "include": {
                            "textures/m_diff_2k.jpg": {
                                "url": "https://dl.polyhaven.org/m_diff.jpg",
                                "size": 700
                            }
                        }
                    }
                }
            }
        });
        let (variant, planned) =
            plan_files(&summary(AssetType::Model), &files, &request()).unwrap();
        assert_eq!(variant, "2k");
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[1].filename, "textures/m_diff_2k.jpg");
    }

    #[test]
    fn a_model_format_cannot_be_one_of_its_texture_maps() {
        // Poly Haven puts a model's maps at the top level beside its formats.
        let files = json!({
            "blend": {"1k": {"blend": {"url": "https://dl.polyhaven.org/m.blend", "size": 10}}},
            "cart_diff": {"1k": {"png": {"url": "https://dl.polyhaven.org/d.png", "size": 5}}}
        });
        let mut asked = request();
        asked.format = Some("cart_diff".into());
        let error = plan_files(&summary(AssetType::Model), &files, &asked).unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetNotFound);

        // The real formats still work.
        let (_, planned) = plan_files(&summary(AssetType::Model), &files, &request()).unwrap();
        assert_eq!(planned[0].filename, "rocky_1k.blend");
    }

    #[test]
    fn variants_are_listed_smallest_first() {
        let files = json!({
            "hdri": {
                "16k": {"hdr": {"url": "https://dl.polyhaven.org/a.hdr"}},
                "1k":  {"hdr": {"url": "https://dl.polyhaven.org/b.hdr"}},
                "24k": {"hdr": {"url": "https://dl.polyhaven.org/c.hdr"}},
                "2k":  {"hdr": {"url": "https://dl.polyhaven.org/d.hdr"}}
            }
        });
        let ids: Vec<String> = variants_from_files(&files, AssetType::Hdri)
            .into_iter()
            .map(|variant| variant.id)
            .collect();
        assert_eq!(
            ids,
            ["1k", "2k", "16k", "24k"],
            "sorted by size, not alphabetically"
        );
    }

    #[test]
    fn every_asset_carries_its_licence() {
        let summary = summary_from_entry(
            "rocky",
            &json!({"name": "Rocky Terrain", "type": 1, "categories": ["outdoor"], "tags": ["rock"]}),
        )
        .unwrap();
        let license = summary.license.unwrap();
        assert_eq!(license.id, "CC0");
        assert_eq!(license.commercial_use, Some(true));
        assert_eq!(summary.asset_type, AssetType::Texture);
        assert_eq!(summary.provider_id, "rocky");
    }

    #[test]
    fn an_unknown_type_code_is_skipped_rather_than_guessed() {
        assert!(summary_from_entry("x", &json!({"name": "X", "type": 99})).is_none());
        assert!(summary_from_entry("x", &json!({"name": "X"})).is_none());
    }

    #[tokio::test]
    async fn search_filters_the_full_listing_and_sorts_it() {
        let fetcher = Arc::new(StubFetcher::new().json(
            "https://api.polyhaven.com/assets?t=textures",
            json!({
                "zebra_wood": {"name": "Zebra Wood", "type": 1, "tags": ["wood"]},
                "alder_wood": {"name": "Alder Wood", "type": 1, "tags": ["wood"]},
                "concrete_01": {"name": "Concrete", "type": 1, "tags": ["concrete"]}
            }),
        ));
        let provider = PolyHaven::new(fetcher);

        let query = SearchAssets {
            query: Some("wood".into()),
            asset_type: Some(AssetType::Texture),
            ..Default::default()
        };
        let results = provider.search(&query).await.unwrap();
        assert_eq!(
            results.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            vec!["Alder Wood", "Zebra Wood"],
            "sorted, so paging over the results is stable"
        );
    }

    #[tokio::test]
    async fn a_missing_asset_is_reported_as_such() {
        let fetcher = Arc::new(StubFetcher::new().json(
            "https://api.polyhaven.com/info/nope",
            json!({"error": "not found"}),
        ));
        let provider = PolyHaven::new(fetcher);
        let error = provider.get("nope").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetNotFound);
    }

    #[test]
    fn the_provider_never_claims_credentials_it_does_not_need() {
        let info = PolyHaven::new(Arc::new(StubFetcher::new())).info();
        assert!(!info.requires_auth);
        assert!(info.license_summary.is_some());
    }
}

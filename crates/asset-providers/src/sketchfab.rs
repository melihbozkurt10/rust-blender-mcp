//! Sketchfab.
//!
//! Search is public; downloading needs an API token, which is read from
//! `BLENDER_MCP_SKETCHFAB_TOKEN` and never appears in a result or a log line.
//!
//! Sketchfab hosts work under many different licences, including ones that
//! forbid commercial use or redistribution. Every licence is passed through
//! exactly as the provider states it, and nothing here decides on the user's
//! behalf that an asset may be used.

use std::sync::Arc;

use blender_protocol::{
    BlenderError, ErrorCode, Result,
    asset::{
        AssetSummary, AssetType, AssetVariant, DownloadAsset, License, ProviderInfo, SearchAssets,
    },
};
use serde_json::Value;

use crate::{
    credentials::Secret,
    http::{Authorization, Fetcher},
    provider::{AssetProvider, BoxFuture, DownloadPlan, PlannedFile, asset_id},
};

pub const ID: &str = "sketchfab";
pub const TOKEN_VARIABLE: &str = "BLENDER_MCP_SKETCHFAB_TOKEN";
const API: &str = "https://api.sketchfab.com/v3";

/// Formats Sketchfab will hand back, most useful first.
const DOWNLOAD_FORMATS: &[&str] = &["gltf", "glb", "usdz", "source"];

pub struct Sketchfab {
    fetcher: Arc<dyn Fetcher>,
    token: Option<Secret>,
}

impl Sketchfab {
    pub fn new(fetcher: Arc<dyn Fetcher>, token: Option<Secret>) -> Self {
        Self { fetcher, token }
    }

    /// Read the token from the environment.
    pub fn from_env(fetcher: Arc<dyn Fetcher>) -> Self {
        Self::new(fetcher, Secret::from_env(TOKEN_VARIABLE))
    }

    pub fn is_configured(&self) -> bool {
        self.token.is_some()
    }

    fn require_token(&self) -> Result<Authorization> {
        self.authorization().ok_or_else(|| {
            BlenderError::new(
                ErrorCode::AssetAuthRequired,
                format!(
                    "Downloading from Sketchfab needs an API token. Set {TOKEN_VARIABLE} in the \
                     server's environment and reconnect. Searching works without one."
                ),
            )
            .with_detail("environment_variable", TOKEN_VARIABLE)
        })
    }
}

/// Sketchfab's licence object, passed through.
///
/// The two booleans are only set for slugs whose terms are unambiguous. An
/// unrecognised slug leaves both unset, because "the provider did not say" and
/// "the provider said no" must not look the same to a caller deciding whether
/// it may ship an asset.
pub(crate) fn license_from(value: &Value) -> Option<License> {
    let object = value.as_object()?;
    let slug = object
        .get("slug")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if slug.is_empty() {
        return None;
    }

    let (attribution, commercial) = match slug.as_str() {
        "cc0" => (Some(false), Some(true)),
        "by" | "by-sa" | "by-nd" => (Some(true), Some(true)),
        "by-nc" | "by-nc-sa" | "by-nc-nd" => (Some(true), Some(false)),
        _ => (None, None),
    };

    Some(License {
        id: slug,
        name: object
            .get("label")
            .or_else(|| object.get("fullName"))
            .and_then(Value::as_str)
            .map(str::to_string),
        url: object
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        requires_attribution: attribution,
        commercial_use: commercial,
    })
}

fn thumbnail(value: &Value) -> Option<String> {
    let images = value.get("thumbnails")?.get("images")?.as_array()?;
    // The largest image under 1024px wide: big enough to recognise, small
    // enough not to be a download in itself.
    images
        .iter()
        .filter(|image| {
            image
                .get("width")
                .and_then(Value::as_u64)
                .is_some_and(|width| width <= 1024)
        })
        .max_by_key(|image| image.get("width").and_then(Value::as_u64).unwrap_or(0))
        .or_else(|| images.first())
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn names(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .or_else(|| entry.get("slug"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Turn one search result or model response into a summary.
pub(crate) fn summary_from_model(model: &Value) -> Option<AssetSummary> {
    let uid = model.get("uid")?.as_str()?.to_string();
    // A model the provider marks as not downloadable is not a result: offering
    // it would only produce a failure at download time. A response that does
    // not mention the field at all is taken at face value.
    if model.get("isDownloadable").is_some_and(|v| v != true) {
        return None;
    }

    Some(AssetSummary {
        id: asset_id(ID, &uid),
        provider_id: uid.clone(),
        provider: ID.to_string(),
        title: model
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_string(),
        asset_type: AssetType::Model,
        authors: model
            .get("user")
            .and_then(|user| {
                user.get("displayName")
                    .or_else(|| user.get("username"))
                    .and_then(Value::as_str)
            })
            .map(|name| vec![name.to_string()]),
        source_url: model
            .get("viewerUrl")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(format!("https://sketchfab.com/3d-models/{uid}"))),
        thumbnail_url: thumbnail(model),
        license: model.get("license").and_then(license_from),
        categories: names(model, "categories"),
        tags: names(model, "tags"),
        variants: vec![],
        // Every Sketchfab download needs a token, even for a free model.
        requires_auth: true,
    })
}

fn search_url(query: &SearchAssets) -> String {
    let mut url = format!(
        "{API}/search?type=models&downloadable=true&count={}",
        query.page.effective_limit().min(24)
    );
    if let Some(text) = &query.query {
        url.push_str("&q=");
        url.push_str(&urlencode(text));
    }
    for category in &query.categories {
        url.push_str("&categories=");
        url.push_str(&urlencode(category));
    }
    if let Some(cursor) = &query.page.cursor {
        url.push_str("&cursor=");
        url.push_str(&urlencode(cursor));
    }
    url
}

/// Percent-encode everything that is not unreserved.
///
/// A hand-rolled encoder rather than a dependency: the rule is short, and being
/// conservative here is what keeps a search term from becoming extra query
/// parameters.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Pick the download format from a `/download` response.
pub(crate) fn pick_download(
    response: &Value,
    wanted: Option<&str>,
) -> Result<(String, String, Option<u64>)> {
    let object = response.as_object().ok_or_else(|| {
        BlenderError::new(
            ErrorCode::AssetProviderError,
            "Sketchfab returned a download response that is not an object.",
        )
    })?;

    let available: Vec<&String> = object
        .iter()
        .filter(|(_, value)| value.get("url").and_then(Value::as_str).is_some())
        .map(|(key, _)| key)
        .collect();

    let chosen = match wanted {
        Some(wanted) => available
            .iter()
            .find(|key| key.eq_ignore_ascii_case(wanted))
            .map(|key| (*key).clone()),
        None => DOWNLOAD_FORMATS.iter().find_map(|preferred| {
            available
                .iter()
                .find(|key| key.eq_ignore_ascii_case(preferred))
                .map(|key| (*key).clone())
        }),
    };

    let chosen = chosen.ok_or_else(|| {
        BlenderError::new(
            ErrorCode::UnsupportedFormat,
            "Sketchfab does not offer this model in the requested format.",
        )
        .with_detail_json(
            "available_formats",
            &available.iter().map(|key| key.as_str()).collect::<Vec<_>>(),
        )
    })?;

    let entry = &object[&chosen];
    let url = entry
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                "Sketchfab returned a download entry with no URL.",
            )
        })?
        .to_string();
    let size = entry.get("size").and_then(Value::as_u64);
    Ok((chosen, url, size))
}

impl AssetProvider for Sketchfab {
    fn id(&self) -> &'static str {
        ID
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            name: "Sketchfab".into(),
            url: Some("https://sketchfab.com".into()),
            asset_types: vec![AssetType::Model],
            authenticated: self.is_configured(),
            requires_auth: true,
            supported_filters: vec!["query".into(), "categories".into(), "format".into()],
            license_summary: Some(
                "Sketchfab models carry a per-model licence, from CC0 to non-commercial and \
                 no-derivatives terms. Read the licence on each asset before using it; this \
                 server reports what the provider states and decides nothing on your behalf."
                    .into(),
            ),
        }
    }

    fn authorization(&self) -> Option<Authorization> {
        self.token.clone().map(Authorization::token)
    }

    fn search<'a>(&'a self, query: &'a SearchAssets) -> BoxFuture<'a, Result<Vec<AssetSummary>>> {
        Box::pin(async move {
            // The token is sent when there is one: an authenticated search sees
            // models a user owns or has bought. It is not required.
            let auth = self.authorization();
            let response = self
                .fetcher
                .get_json(&search_url(query), auth.as_ref())
                .await?;

            let results = response
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    BlenderError::new(
                        ErrorCode::AssetProviderError,
                        "Sketchfab returned a search response with no results array.",
                    )
                })?;

            Ok(results.iter().filter_map(summary_from_model).collect())
        })
    }

    fn get<'a>(&'a self, provider_asset_id: &'a str) -> BoxFuture<'a, Result<AssetSummary>> {
        Box::pin(async move {
            let auth = self.authorization();
            let model = self
                .fetcher
                .get_json(&format!("{API}/models/{provider_asset_id}"), auth.as_ref())
                .await?;
            let mut summary = summary_from_model(&model).ok_or_else(|| {
                BlenderError::new(
                    ErrorCode::AssetNotFound,
                    format!("Sketchfab has no downloadable model with uid `{provider_asset_id}`."),
                )
                .with_detail("asset_id", provider_asset_id)
            })?;

            if let Some(archives) = model.get("archives").and_then(Value::as_object) {
                summary.variants = archives
                    .iter()
                    .map(|(format, entry)| AssetVariant {
                        id: format.clone(),
                        resolution: None,
                        format: Some(format.clone()),
                        size_bytes: entry.get("size").and_then(Value::as_u64),
                        map: None,
                    })
                    .collect();
            }
            Ok(summary)
        })
    }

    fn plan<'a>(&'a self, request: &'a DownloadAsset) -> BoxFuture<'a, Result<DownloadPlan>> {
        Box::pin(async move {
            let auth = self.require_token()?;
            let asset = self.get(&request.asset_id).await?;

            // This endpoint mints a short-lived signed URL. The token buys the
            // URL; the URL itself must be fetched without it, so the token is
            // never sent to the CDN.
            let response = self
                .fetcher
                .get_json(
                    &format!("{API}/models/{}/download", request.asset_id),
                    Some(&auth),
                )
                .await?;

            let wanted = request.format.as_deref().or(request.variant.as_deref());
            let (variant, url, size) = pick_download(&response, wanted)?;

            // Sketchfab serves every format as a zip archive.
            let filename = format!("{}_{variant}.zip", asset.provider_id);
            Ok(DownloadPlan {
                asset,
                variant,
                files: vec![PlannedFile::new(url, filename).with_size(size)],
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::http::stub::StubFetcher;

    fn model() -> Value {
        json!({
            "uid": "abc123",
            "name": "Wooden Chair",
            "isDownloadable": true,
            "viewerUrl": "https://sketchfab.com/3d-models/wooden-chair-abc123",
            "user": {"displayName": "Someone", "username": "someone"},
            "license": {"slug": "by", "label": "CC Attribution", "url": "https://creativecommons.org/licenses/by/4.0/"},
            "thumbnails": {"images": [
                {"url": "https://media.sketchfab.com/big.jpg", "width": 2048},
                {"url": "https://media.sketchfab.com/med.jpg", "width": 720},
                {"url": "https://media.sketchfab.com/small.jpg", "width": 200}
            ]},
            "tags": [{"name": "chair"}, {"name": "furniture"}],
            "categories": [{"name": "Furniture"}]
        })
    }

    fn request() -> DownloadAsset {
        DownloadAsset {
            provider: ID.into(),
            asset_id: "abc123".into(),
            variant: None,
            resolution: None,
            format: None,
            maps: vec![],
            force: false,
        }
    }

    #[test]
    fn a_model_summary_carries_the_licence_verbatim() {
        let summary = summary_from_model(&model()).unwrap();
        assert_eq!(summary.provider_id, "abc123");
        assert_eq!(summary.title, "Wooden Chair");
        assert!(summary.requires_auth, "downloads always need a token");
        let license = summary.license.unwrap();
        assert_eq!(license.id, "by");
        assert_eq!(license.name.as_deref(), Some("CC Attribution"));
        assert_eq!(license.requires_attribution, Some(true));
    }

    #[test]
    fn an_unrecognised_licence_stays_unstated_rather_than_permissive() {
        let license = license_from(&json!({"slug": "st", "label": "Standard"})).unwrap();
        assert_eq!(license.id, "st");
        assert_eq!(
            license.commercial_use, None,
            "an unknown licence must not be reported as permitting anything"
        );
        assert_eq!(license.requires_attribution, None);

        let nc = license_from(&json!({"slug": "by-nc-nd"})).unwrap();
        assert_eq!(nc.commercial_use, Some(false));
    }

    #[test]
    fn the_thumbnail_is_a_preview_not_a_download() {
        let url = summary_from_model(&model()).unwrap().thumbnail_url.unwrap();
        assert_eq!(url, "https://media.sketchfab.com/med.jpg");
    }

    #[test]
    fn a_search_term_cannot_smuggle_extra_query_parameters() {
        let query = SearchAssets {
            query: Some("chair&downloadable=false&type=collections".into()),
            ..Default::default()
        };
        let url = search_url(&query);
        assert_eq!(
            url.matches("downloadable=").count(),
            1,
            "the term was encoded, not appended as a parameter: {url}"
        );
        assert!(url.contains("chair%26downloadable"));
    }

    #[test]
    fn a_download_response_prefers_gltf_and_reports_what_exists() {
        let response = json!({
            "gltf": {"url": "https://cdn.sketchfab.com/g.zip", "size": 1234},
            "usdz": {"url": "https://cdn.sketchfab.com/u.zip", "size": 999}
        });
        let (format, url, size) = pick_download(&response, None).unwrap();
        assert_eq!(format, "gltf");
        assert_eq!(url, "https://cdn.sketchfab.com/g.zip");
        assert_eq!(size, Some(1234));

        let (format, ..) = pick_download(&response, Some("usdz")).unwrap();
        assert_eq!(format, "usdz");

        let error = pick_download(&response, Some("fbx")).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedFormat);
        assert!(error.details["available_formats"].as_array().is_some());
    }

    #[tokio::test]
    async fn downloading_without_a_token_says_exactly_what_is_missing() {
        let provider = Sketchfab::new(Arc::new(StubFetcher::new()), None);
        let error = provider.plan(&request()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetAuthRequired);
        assert_eq!(error.details["environment_variable"], TOKEN_VARIABLE);
        assert!(
            !provider.info().authenticated,
            "an unconfigured provider must not claim to be authenticated"
        );
    }

    #[tokio::test]
    async fn the_token_reaches_the_api_and_not_the_cdn() {
        let fetcher = Arc::new(
            StubFetcher::new()
                .json("https://api.sketchfab.com/v3/models/abc123", model())
                .json(
                    "https://api.sketchfab.com/v3/models/abc123/download",
                    json!({"gltf": {"url": "https://cdn.sketchfab.com/signed.zip", "size": 10}}),
                ),
        );
        let provider = Sketchfab::new(
            Arc::clone(&fetcher) as Arc<dyn Fetcher>,
            Some(Secret::new("token-value-12345")),
        );

        let plan = provider.plan(&request()).await.unwrap();
        assert_eq!(plan.variant, "gltf");
        assert_eq!(plan.files[0].filename, "abc123_gltf.zip");
        assert!(
            !plan.files[0].authenticated,
            "the signed URL must be fetched without the token"
        );
        assert!(
            fetcher
                .authorized()
                .iter()
                .all(|url| url.starts_with("https://api.sketchfab.com/")),
            "the token went somewhere it should not have: {:?}",
            fetcher.authorized()
        );
    }

    #[tokio::test]
    async fn an_error_never_carries_the_token() {
        let provider = Sketchfab::new(
            Arc::new(StubFetcher::new()),
            Some(Secret::new("sk-secret-token-1")),
        );
        let error = provider.get("missing").await.unwrap_err();
        let rendered = format!("{error:?} {}", serde_json::to_string(&error).unwrap());
        assert!(!rendered.contains("sk-secret-token-1"), "got {rendered}");
    }
}

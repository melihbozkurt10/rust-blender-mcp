//! External asset libraries behind one trait.
//!
//! The rules that matter, all enforced here rather than left to each provider:
//!
//! * Credentials come from the environment, live in a [`credentials::Secret`],
//!   and are sent only to the provider's own API -- never to a CDN, never into
//!   a tool result, never into a log line.
//! * Downloads are HTTPS-only, size-capped, extension-checked, and land inside
//!   one managed directory.
//! * Nothing downloaded is executed, and no add-on is ever installed from a
//!   provider.
//! * Licence metadata is reported exactly as the provider states it. There is
//!   no "free to use" flag anywhere in this crate, because that is a legal
//!   conclusion about a specific project and not a property of an asset.

// The other six crates `forbid(unsafe_code)` outright. This one cannot, because
// `env::set_var` is unsafe in edition 2024 and two tests need it -- but the
// shipped code is held to the same rule, which is what the guarantee is about.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

pub mod credentials;
pub mod download;
pub mod http;
pub mod policy;
pub mod polyhaven;
pub mod provider;
pub mod sketchfab;

use std::{path::PathBuf, sync::Arc};

use blender_protocol::{
    BlenderError, ErrorCode, Result,
    asset::{
        AssetSummary, DownloadAsset, DownloadedAsset, ProviderInfo, SearchAssets, check_asset_id,
        check_provider_id,
    },
};
use serde::{Deserialize, Serialize};

pub use crate::{
    credentials::Secret,
    download::Downloader,
    http::{Fetcher, HttpFetcher},
    policy::DownloadPolicy,
    provider::{AssetProvider, DownloadPlan, PlannedFile, asset_id},
};

/// How the provider set is configured.
#[derive(Debug, Clone)]
pub struct Config {
    /// The managed directory downloads land in. Nothing is written outside it.
    pub downloads_root: PathBuf,
    pub policy: DownloadPolicy,
    /// Sketchfab API token, if one is configured.
    pub sketchfab_token: Option<Secret>,
    /// Provider ids to enable. Empty enables every provider that can work with
    /// the credentials available.
    pub enabled: Vec<String>,
}

impl Config {
    pub fn new(downloads_root: PathBuf) -> Self {
        Self {
            downloads_root,
            policy: DownloadPolicy::default(),
            sketchfab_token: None,
            enabled: Vec::new(),
        }
    }

    /// Read credentials from the environment.
    ///
    /// Tokens are never read from tool arguments: a caller must not be able to
    /// make this server authenticate as someone else, and a token in an
    /// argument would end up in a transcript.
    pub fn with_env_credentials(mut self) -> Self {
        self.sketchfab_token = Secret::from_env(sketchfab::TOKEN_VARIABLE);
        self
    }

    fn is_enabled(&self, id: &str) -> bool {
        self.enabled.is_empty() || self.enabled.iter().any(|name| name == id)
    }
}

/// Something that went wrong with one provider during a search across several.
///
/// One provider being down, rate limited or unconfigured must not turn a search
/// into a total failure -- but it must not be silent either, or a caller reads
/// an incomplete list as a complete one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderWarning {
    pub provider: String,
    pub code: String,
    pub message: String,
}

/// What a search returned.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchResults {
    pub assets: Vec<AssetSummary>,
    /// Cursor for the next page, absent when this is the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Total matches, when every provider could be enumerated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    pub providers_searched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ProviderWarning>,
}

/// The configured providers, and the downloader they share.
pub struct AssetProviders {
    providers: Vec<Arc<dyn AssetProvider>>,
    downloader: Downloader,
}

impl AssetProviders {
    /// Build the real set: Poly Haven, plus Sketchfab when a token is present.
    pub fn new(config: &Config) -> Result<Self> {
        let policy = Arc::new(config.policy.clone());
        let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new(Arc::clone(&policy))?);
        Ok(Self::with_fetcher(config, fetcher))
    }

    /// Build the set over a given fetcher. The seam tests use.
    pub fn with_fetcher(config: &Config, fetcher: Arc<dyn Fetcher>) -> Self {
        let mut providers: Vec<Arc<dyn AssetProvider>> = Vec::new();

        if config.is_enabled(polyhaven::ID) {
            providers.push(Arc::new(polyhaven::PolyHaven::new(Arc::clone(&fetcher))));
        }
        if config.is_enabled(sketchfab::ID) {
            // Registered even without a token: searching works, and a download
            // then fails with an error that says which variable to set. Hiding
            // the provider entirely would make that undiagnosable.
            providers.push(Arc::new(sketchfab::Sketchfab::new(
                Arc::clone(&fetcher),
                config.sketchfab_token.clone(),
            )));
        }

        let downloader = Downloader::new(
            fetcher,
            Arc::new(config.policy.clone()),
            config.downloads_root.clone(),
        );

        Self {
            providers,
            downloader,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers
            .iter()
            .map(|provider| provider.info())
            .collect()
    }

    pub fn downloads_root(&self) -> &std::path::Path {
        self.downloader.root()
    }

    fn provider(&self, id: &str) -> Result<&Arc<dyn AssetProvider>> {
        check_provider_id(id)?;
        self.providers
            .iter()
            .find(|provider| provider.id() == id)
            .ok_or_else(|| {
                BlenderError::new(
                    ErrorCode::AssetProviderError,
                    format!("There is no asset provider called `{id}`."),
                )
                .with_detail("provider", id)
                .with_detail_json(
                    "available_providers",
                    &self
                        .providers
                        .iter()
                        .map(|provider| provider.id())
                        .collect::<Vec<_>>(),
                )
            })
    }

    /// Search one provider, or every provider.
    pub async fn search(&self, query: &SearchAssets) -> Result<SearchResults> {
        let targets: Vec<&Arc<dyn AssetProvider>> = match &query.provider {
            Some(id) => vec![self.provider(id)?],
            None => self.providers.iter().collect(),
        };
        if targets.is_empty() {
            return Err(BlenderError::new(
                ErrorCode::AssetProviderError,
                "No asset providers are configured.",
            ));
        }

        let mut assets = Vec::new();
        let mut warnings = Vec::new();
        let mut searched = Vec::new();

        for provider in &targets {
            searched.push(provider.id().to_string());
            match provider.search(query).await {
                Ok(found) => assets.extend(found),
                // A provider that fails is reported alongside the results that
                // did arrive, so the caller can tell "nothing matched" from
                // "one library did not answer".
                Err(error) => warnings.push(ProviderWarning {
                    provider: provider.id().to_string(),
                    code: error.code.as_str().to_string(),
                    message: error.message.clone(),
                }),
            }
        }

        let total = assets.len();
        let offset = parse_cursor(query.page.cursor.as_deref())?;
        let limit = query.page.effective_limit() as usize;

        let page: Vec<AssetSummary> = assets.into_iter().skip(offset).take(limit).collect();
        let next = offset + page.len();
        let next_cursor = (next < total).then(|| next.to_string());

        Ok(SearchResults {
            assets: page,
            next_cursor,
            total: Some(total),
            providers_searched: searched,
            warnings,
        })
    }

    /// One asset's full detail.
    pub async fn get(&self, provider: &str, asset: &str) -> Result<AssetSummary> {
        check_asset_id(asset)?;
        self.provider(provider)?.get(asset).await
    }

    /// Work out what a download would fetch, without fetching it.
    pub async fn plan(&self, request: &DownloadAsset) -> Result<DownloadPlan> {
        check_asset_id(&request.asset_id)?;
        self.provider(&request.provider)?.plan(request).await
    }

    /// Fetch an asset into the managed downloads directory.
    pub async fn download(&self, request: &DownloadAsset) -> Result<DownloadedAsset> {
        let provider = self.provider(&request.provider)?;
        check_asset_id(&request.asset_id)?;
        let plan = provider.plan(request).await?;
        let auth = provider.authorization();
        self.downloader
            .run(&plan, auth.as_ref(), request.force)
            .await
    }
}

/// Cursors are an offset into the merged result list.
fn parse_cursor(cursor: Option<&str>) -> Result<usize> {
    match cursor {
        None => Ok(0),
        Some(value) => value.trim().parse().map_err(|_| {
            BlenderError::invalid_argument(
                "`cursor` must be a value returned as `next_cursor` by a previous search.",
            )
            .with_detail("cursor", value)
        }),
    }
}

#[cfg(test)]
mod tests {
    use blender_protocol::asset::AssetType;
    use serde_json::json;

    use super::*;
    use crate::http::stub::StubFetcher;

    fn config() -> Config {
        Config::new(std::env::temp_dir().join("blender-mcp-registry-test"))
    }

    fn listing() -> serde_json::Value {
        json!({
            "wood_a": {"name": "Wood A", "type": 1, "tags": ["wood"]},
            "wood_b": {"name": "Wood B", "type": 1, "tags": ["wood"]},
            "wood_c": {"name": "Wood C", "type": 1, "tags": ["wood"]}
        })
    }

    fn providers(fetcher: Arc<dyn Fetcher>) -> AssetProviders {
        AssetProviders::with_fetcher(&config(), fetcher)
    }

    #[test]
    fn sketchfab_is_listed_even_without_a_token() {
        let set = providers(Arc::new(StubFetcher::new()));
        let ids: Vec<String> = set.list().into_iter().map(|info| info.id).collect();
        assert_eq!(ids, vec!["polyhaven", "sketchfab"]);

        let sketchfab = set
            .list()
            .into_iter()
            .find(|info| info.id == "sketchfab")
            .unwrap();
        assert!(sketchfab.requires_auth);
        assert!(
            !sketchfab.authenticated,
            "no token was configured, and the listing must say so"
        );
    }

    #[test]
    fn providers_can_be_restricted_by_configuration() {
        let config = Config {
            enabled: vec!["polyhaven".into()],
            ..config()
        };
        let set = AssetProviders::with_fetcher(&config, Arc::new(StubFetcher::new()));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn an_unknown_provider_lists_the_ones_that_exist() {
        let set = providers(Arc::new(StubFetcher::new()));
        let error = set.provider("polyhavn").map(|_| ()).unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetProviderError);
        assert!(
            error.details["available_providers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "polyhaven")
        );
    }

    #[tokio::test]
    async fn results_are_paged_with_a_cursor() {
        let fetcher = Arc::new(
            StubFetcher::new().json("https://api.polyhaven.com/assets?t=textures", listing()),
        );
        let set = providers(fetcher);

        let query = SearchAssets {
            provider: Some("polyhaven".into()),
            query: Some("wood".into()),
            asset_type: Some(AssetType::Texture),
            page: blender_protocol::Page {
                limit: Some(2),
                cursor: None,
            },
            ..Default::default()
        };
        let first = set.search(&query).await.unwrap();
        assert_eq!(first.assets.len(), 2);
        assert_eq!(first.total, Some(3));
        assert_eq!(first.next_cursor.as_deref(), Some("2"));

        let second = SearchAssets {
            page: blender_protocol::Page {
                limit: Some(2),
                cursor: first.next_cursor.clone(),
            },
            ..query
        };
        let second = set.search(&second).await.unwrap();
        assert_eq!(second.assets.len(), 1);
        assert!(second.next_cursor.is_none(), "the last page says so");
        assert_eq!(second.assets[0].title, "Wood C");
    }

    #[tokio::test]
    async fn one_failing_provider_does_not_sink_the_search() {
        // Poly Haven answers; Sketchfab has no stubbed response and fails.
        let fetcher =
            Arc::new(StubFetcher::new().json("https://api.polyhaven.com/assets", listing()));
        let set = providers(fetcher);

        let query = SearchAssets {
            query: Some("wood".into()),
            ..Default::default()
        };
        let results = set.search(&query).await.unwrap();
        assert_eq!(results.assets.len(), 3);
        assert_eq!(results.warnings.len(), 1);
        assert_eq!(results.warnings[0].provider, "sketchfab");
        assert_eq!(
            results.providers_searched,
            vec!["polyhaven".to_string(), "sketchfab".to_string()],
            "the caller can see which libraries were consulted"
        );
    }

    #[tokio::test]
    async fn a_nonsense_cursor_is_rejected() {
        let fetcher =
            Arc::new(StubFetcher::new().json("https://api.polyhaven.com/assets", listing()));
        let query = SearchAssets {
            query: Some("wood".into()),
            page: blender_protocol::Page {
                limit: None,
                cursor: Some("../../etc".into()),
            },
            ..Default::default()
        };
        let error = providers(fetcher).search(&query).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn a_download_id_is_validated_before_any_request() {
        let fetcher = Arc::new(StubFetcher::new());
        let set = providers(Arc::clone(&fetcher) as Arc<dyn Fetcher>);
        let request = DownloadAsset {
            provider: "polyhaven".into(),
            asset_id: "../../../etc/passwd".into(),
            variant: None,
            resolution: None,
            format: None,
            maps: vec![],
            force: false,
        };
        assert!(set.download(&request).await.is_err());
        assert!(fetcher.requests().is_empty(), "nothing was fetched");
    }
}

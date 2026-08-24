//! Turning a plan into files on disk, with a cache in front of it.
//!
//! Downloads land under one managed root and nowhere else. Every path segment
//! is built from validated components, and the finished path is checked to be
//! inside the root before anything is written, so a provider that returns a
//! hostile asset id or filename cannot reach the rest of the filesystem.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use blender_protocol::{
    BlenderError, ErrorCode, Result,
    asset::{DownloadedAsset, DownloadedFile, check_asset_id, check_provider_id},
};

use crate::{
    http::{Authorization, Fetcher},
    policy::DownloadPolicy,
    provider::DownloadPlan,
};

/// Fetches planned files into the managed downloads root.
pub struct Downloader {
    fetcher: Arc<dyn Fetcher>,
    policy: Arc<DownloadPolicy>,
    root: PathBuf,
}

/// What the cache holds for one variant of one asset.
const MANIFEST: &str = "manifest.json";

impl Downloader {
    pub fn new(fetcher: Arc<dyn Fetcher>, policy: Arc<DownloadPolicy>, root: PathBuf) -> Self {
        Self {
            fetcher,
            policy,
            root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fetch everything in a plan, or return the cached copy.
    pub async fn run(
        &self,
        plan: &DownloadPlan,
        auth: Option<&Authorization>,
        force: bool,
    ) -> Result<DownloadedAsset> {
        let directory =
            self.directory_for(&plan.asset.provider, &plan.asset.provider_id, &plan.variant)?;

        if !force && let Some(cached) = self.cached(&directory).await {
            let total = cached.iter().map(|file| file.size_bytes).sum();
            return Ok(DownloadedAsset {
                asset: plan.asset.clone(),
                files: cached,
                from_cache: true,
                total_bytes: Some(total),
            });
        }

        // Refuse an oversized download before spending bandwidth on it, when
        // the provider was willing to say how big it is.
        let declared: u64 = plan.files.iter().filter_map(|file| file.size_bytes).sum();
        if declared > 0 {
            self.policy.check_total_size(declared)?;
        }

        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| {
                BlenderError::new(
                    ErrorCode::AssetDownloadFailed,
                    format!("Could not create the download directory: {error}."),
                )
                .with_detail("path", directory.display().to_string())
            })?;

        let mut files = Vec::with_capacity(plan.files.len());
        let mut total: u64 = 0;

        for planned in &plan.files {
            let parts = self.policy.safe_relative_path(&planned.filename)?;
            let destination = self.inside_root(
                parts
                    .iter()
                    .fold(directory.clone(), |path, part| path.join(part)),
            )?;
            if let Some(size) = planned.size_bytes {
                self.policy.check_size(size)?;
            }
            // A model archive puts its textures in a subdirectory of its own.
            if let Some(parent) = destination.parent()
                && parent != directory
            {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    BlenderError::new(
                        ErrorCode::AssetDownloadFailed,
                        format!("Could not create the download directory: {error}."),
                    )
                })?;
            }

            let auth = if planned.authenticated { auth } else { None };
            let fetched = self
                .fetcher
                .download(&planned.url, auth, &destination)
                .await?;

            total += fetched.size_bytes;
            self.policy.check_total_size(total)?;

            files.push(DownloadedFile {
                path: self.relative(&destination),
                size_bytes: fetched.size_bytes,
                sha256: fetched.sha256,
                map: planned.map.clone(),
                mime_type: fetched.mime_type,
            });
        }

        self.write_manifest(&directory, &files).await;

        Ok(DownloadedAsset {
            asset: plan.asset.clone(),
            files,
            from_cache: false,
            total_bytes: Some(total),
        })
    }

    /// The directory one variant of one asset lives in.
    pub fn directory_for(&self, provider: &str, asset_id: &str, variant: &str) -> Result<PathBuf> {
        check_provider_id(provider)?;
        check_asset_id(asset_id)?;
        let variant = safe_component(variant, "variant")?;
        self.inside_root(self.root.join(provider).join(asset_id).join(variant))
    }

    /// Reject any path that is not under the managed root.
    ///
    /// The components are already validated, so this can only fire if that
    /// validation is ever weakened -- which is exactly when a second check
    /// earns its keep.
    fn inside_root(&self, path: PathBuf) -> Result<PathBuf> {
        if !path.starts_with(&self.root) || path.components().any(is_parent_component) {
            return Err(BlenderError::new(
                ErrorCode::PathNotAllowed,
                "The download path escapes the managed downloads directory.",
            )
            .with_detail("path", path.display().to_string()));
        }
        Ok(path)
    }

    /// A path relative to the downloads root, with forward slashes, so results
    /// look the same on every platform.
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The cached files for a variant, if every one of them is still on disk.
    ///
    /// A manifest listing a file that has since been deleted is not a cache
    /// hit; reporting it as one would hand back a path that does not exist.
    async fn cached(&self, directory: &Path) -> Option<Vec<DownloadedFile>> {
        let manifest = tokio::fs::read(directory.join(MANIFEST)).await.ok()?;
        let files: Vec<DownloadedFile> = serde_json::from_slice(&manifest).ok()?;
        if files.is_empty() {
            return None;
        }
        for file in &files {
            let path = self
                .root
                .join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.len() == file.size_bytes => {}
                _ => return None,
            }
        }
        Some(files)
    }

    /// A failed manifest write costs a re-download, not a wrong answer, so it
    /// is logged rather than propagated.
    async fn write_manifest(&self, directory: &Path, files: &[DownloadedFile]) {
        let Ok(encoded) = serde_json::to_vec_pretty(files) else {
            return;
        };
        if let Err(error) = tokio::fs::write(directory.join(MANIFEST), encoded).await {
            tracing::warn!(%error, "could not write the download manifest; the next request will re-download");
        }
    }
}

fn is_parent_component(component: std::path::Component<'_>) -> bool {
    matches!(component, std::path::Component::ParentDir)
}

/// A single path segment supplied by a provider.
fn safe_component(value: &str, field: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(BlenderError::invalid_argument(format!(
            "`{field}` must be 1 to 64 characters."
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        || trimmed.starts_with('.')
        || trimmed.contains("..")
    {
        return Err(BlenderError::invalid_argument(format!(
            "`{field}` must use letters, digits, dots, dashes and underscores, and must not \
             start with a dot."
        ))
        .with_detail(field, trimmed));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use blender_protocol::asset::{AssetSummary, AssetType};

    use super::*;
    use crate::{
        http::stub::StubFetcher,
        provider::{PlannedFile, asset_id},
    };

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("blender-mcp-download-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn summary() -> AssetSummary {
        AssetSummary {
            id: asset_id("polyhaven", "rocky_terrain"),
            provider_id: "rocky_terrain".into(),
            provider: "polyhaven".into(),
            title: "Rocky Terrain".into(),
            asset_type: AssetType::Hdri,
            authors: None,
            source_url: None,
            thumbnail_url: None,
            license: None,
            categories: vec![],
            tags: vec![],
            variants: vec![],
            requires_auth: false,
        }
    }

    fn plan(files: Vec<PlannedFile>) -> DownloadPlan {
        DownloadPlan {
            asset: summary(),
            variant: "2k".into(),
            files,
        }
    }

    fn downloader(fetcher: Arc<dyn Fetcher>, root: PathBuf) -> Downloader {
        Downloader::new(fetcher, Arc::new(DownloadPolicy::default()), root)
    }

    #[tokio::test]
    async fn a_download_lands_under_the_managed_root() {
        let root = temp_root("basic");
        let fetcher = Arc::new(
            StubFetcher::new().bytes("https://dl.polyhaven.org/a_2k.exr", b"pretend-exr".to_vec()),
        );
        let downloader = downloader(fetcher, root.clone());

        let result = downloader
            .run(
                &plan(vec![PlannedFile::new(
                    "https://dl.polyhaven.org/a_2k.exr",
                    "rocky_terrain_2k.exr",
                )]),
                None,
                false,
            )
            .await
            .unwrap();

        assert!(!result.from_cache);
        assert_eq!(result.files.len(), 1);
        assert_eq!(
            result.files[0].path, "polyhaven/rocky_terrain/2k/rocky_terrain_2k.exr",
            "paths are relative to the root and use forward slashes"
        );
        assert_eq!(result.total_bytes, Some(11));
        assert!(
            root.join("polyhaven/rocky_terrain/2k/rocky_terrain_2k.exr")
                .exists()
        );
        assert_eq!(
            result.files[0].sha256.len(),
            64,
            "a sha256 is recorded so a caller can verify the file"
        );
    }

    #[tokio::test]
    async fn a_model_keeps_its_texture_subdirectory() {
        let root = temp_root("nested");
        let fetcher = Arc::new(
            StubFetcher::new()
                .bytes("https://dl.polyhaven.org/m.blend", b"blend".to_vec())
                .bytes("https://dl.polyhaven.org/t.jpg", b"jpeg".to_vec()),
        );
        let downloader = downloader(fetcher, root.clone());

        let result = downloader
            .run(
                &plan(vec![
                    PlannedFile::new("https://dl.polyhaven.org/m.blend", "model.blend"),
                    PlannedFile::new("https://dl.polyhaven.org/t.jpg", "textures/rock_diff.jpg"),
                ]),
                None,
                false,
            )
            .await
            .unwrap();

        assert_eq!(
            result.files[1].path, "polyhaven/rocky_terrain/2k/textures/rock_diff.jpg",
            "the path the .blend refers to is preserved"
        );
        assert!(
            root.join("polyhaven/rocky_terrain/2k/textures/rock_diff.jpg")
                .exists()
        );
    }

    #[tokio::test]
    async fn a_second_request_is_served_from_the_cache() {
        let root = temp_root("cache");
        let fetcher = Arc::new(
            StubFetcher::new().bytes("https://dl.polyhaven.org/a_2k.exr", b"pretend-exr".to_vec()),
        );
        let downloader = downloader(Arc::clone(&fetcher) as Arc<dyn Fetcher>, root);
        let plan = plan(vec![PlannedFile::new(
            "https://dl.polyhaven.org/a_2k.exr",
            "rocky_terrain_2k.exr",
        )]);

        downloader.run(&plan, None, false).await.unwrap();
        let second = downloader.run(&plan, None, false).await.unwrap();
        assert!(second.from_cache);
        assert_eq!(
            fetcher.requests().len(),
            1,
            "the network was not touched again"
        );

        let forced = downloader.run(&plan, None, true).await.unwrap();
        assert!(!forced.from_cache);
        assert_eq!(fetcher.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_deleted_file_is_not_a_cache_hit() {
        let root = temp_root("gap");
        let fetcher = Arc::new(
            StubFetcher::new().bytes("https://dl.polyhaven.org/a_2k.exr", b"pretend-exr".to_vec()),
        );
        let downloader = downloader(Arc::clone(&fetcher) as Arc<dyn Fetcher>, root.clone());
        let plan = plan(vec![PlannedFile::new(
            "https://dl.polyhaven.org/a_2k.exr",
            "rocky_terrain_2k.exr",
        )]);

        downloader.run(&plan, None, false).await.unwrap();
        std::fs::remove_file(root.join("polyhaven/rocky_terrain/2k/rocky_terrain_2k.exr")).unwrap();

        let again = downloader.run(&plan, None, false).await.unwrap();
        assert!(
            !again.from_cache,
            "the manifest promised a file that is gone"
        );
    }

    #[tokio::test]
    async fn a_hostile_filename_is_refused_before_anything_is_written() {
        let root = temp_root("escape");
        let fetcher = Arc::new(StubFetcher::new().bytes("https://dl.example.com/x", b"x".to_vec()));
        let downloader = downloader(fetcher, root.clone());

        let error = downloader
            .run(
                &plan(vec![PlannedFile::new(
                    "https://dl.example.com/x",
                    "../../../../evil.exr",
                )]),
                None,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetProviderError);
    }

    #[tokio::test]
    async fn an_oversized_declared_download_is_refused_up_front() {
        let root = temp_root("toobig");
        let fetcher = Arc::new(StubFetcher::new().bytes("https://dl.example.com/x", b"x".to_vec()));
        let downloader = Downloader::new(
            fetcher.clone(),
            Arc::new(DownloadPolicy {
                max_bytes: 10,
                max_total_bytes: 10,
                ..DownloadPolicy::default()
            }),
            root,
        );

        let error = downloader
            .run(
                &plan(vec![
                    PlannedFile::new("https://dl.example.com/x", "a.exr").with_size(Some(1_000)),
                ]),
                None,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetDownloadFailed);
        assert!(fetcher.requests().is_empty(), "nothing was fetched");
    }

    #[tokio::test]
    async fn credentials_only_go_to_urls_that_asked_for_them() {
        let root = temp_root("auth");
        let fetcher = Arc::new(
            StubFetcher::new()
                .bytes("https://api.example.com/private", b"a".to_vec())
                .bytes("https://cdn.example.com/signed", b"b".to_vec()),
        );
        let downloader = downloader(Arc::clone(&fetcher) as Arc<dyn Fetcher>, root);
        let auth = Authorization::token(crate::credentials::Secret::new("token-value-1234"));

        downloader
            .run(
                &plan(vec![
                    PlannedFile {
                        authenticated: true,
                        ..PlannedFile::new("https://api.example.com/private", "a.exr")
                    },
                    PlannedFile::new("https://cdn.example.com/signed", "b.exr"),
                ]),
                Some(&auth),
                false,
            )
            .await
            .unwrap();

        assert_eq!(
            fetcher.authorized(),
            vec!["https://api.example.com/private".to_string()],
            "a signed CDN URL must not receive the token"
        );
    }

    #[test]
    fn a_variant_cannot_be_a_path() {
        let downloader = downloader(Arc::new(StubFetcher::new()), PathBuf::from("/downloads"));
        assert!(
            downloader
                .directory_for("polyhaven", "rock", "../../etc")
                .is_err()
        );
        assert!(
            downloader
                .directory_for("polyhaven", "../rock", "2k")
                .is_err()
        );
        assert!(
            downloader
                .directory_for("Poly Haven", "rock", "2k")
                .is_err()
        );
        assert!(downloader.directory_for("polyhaven", "rock", "4k").is_ok());
    }
}

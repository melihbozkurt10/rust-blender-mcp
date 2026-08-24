//! Managed output files.
//!
//! Renders, bakes and exports produce files. Callers never choose where those
//! land: they supply a base name, the server allocates a path inside a managed
//! root, and what comes back is an artifact reference. That is what makes
//! "render to `C:\Windows\System32\...`" unexpressible rather than merely
//! discouraged.
//!
//! Artifacts are also not returned as inline base64. A 40 MB image inside a
//! JSON tool result is unusable to a model and expensive for everyone.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::SystemTime,
};

use blender_protocol::{BlenderError, ErrorCode, ids::ArtifactId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Root, resolve_managed_path};

/// A file the server produced and owns.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Artifact {
    pub artifact_id: ArtifactId,
    /// Absolute path, always inside a managed root.
    pub path: String,
    /// Path relative to its root, which is what a caller should quote.
    pub relative_path: String,
    pub root: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Seconds since the epoch, so a caller can tell one run from another.
    pub created_at: u64,
}

/// The artifact registry.
#[derive(Default)]
pub struct ArtifactStore {
    entries: RwLock<Vec<Artifact>>,
    /// How many artifacts to remember. The files stay on disk; only the index
    /// is bounded.
    capacity: usize,
}

impl ArtifactStore {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(Vec::new()),
            capacity: capacity.max(1),
        })
    }

    /// Allocate a path for a new output without creating the file.
    ///
    /// The name is sanitised, made unique against what is already there, and
    /// joined inside the root. Blender is then told the absolute path.
    pub fn allocate(
        &self,
        config: &Config,
        root: Root,
        base_name: &str,
        extension: &str,
    ) -> Result<PathBuf, BlenderError> {
        blender_protocol::render::check_artifact_name(base_name)?;
        let root_path = config.root_path(root);
        std::fs::create_dir_all(&root_path).map_err(|error| {
            BlenderError::new(
                ErrorCode::PermissionDenied,
                format!("Could not prepare `{}`: {error}", root_path.display()),
            )
        })?;

        let stem = sanitise(base_name);
        let mut candidate = format!("{stem}.{extension}");
        let mut counter = 1;
        while root_path.join(&candidate).exists() {
            candidate = format!("{stem}_{counter:03}.{extension}");
            counter += 1;
            if counter > 9999 {
                return Err(BlenderError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "Cannot find an unused filename for `{stem}` in {}.",
                        root.id()
                    ),
                ));
            }
        }

        resolve_managed_path(&root_path, &candidate)
    }

    /// Record a file that now exists, reading its size from disk.
    pub fn register(
        &self,
        config: &Config,
        root: Root,
        path: &Path,
        mime_type: &str,
    ) -> Result<Artifact, BlenderError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            BlenderError::new(
                ErrorCode::BlenderInternalError,
                format!(
                    "Blender reported writing `{}`, but it is not there: {error}",
                    path.display()
                ),
            )
            .with_detail("path", path.display().to_string())
        })?;

        let root_path = config.root_path(root);
        let relative = path
            .strip_prefix(&root_path)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

        let artifact = Artifact {
            artifact_id: ArtifactId::new(),
            path: path.display().to_string(),
            relative_path: relative,
            root: root.id().to_string(),
            mime_type: mime_type.to_string(),
            size_bytes: metadata.len(),
            width: None,
            height: None,
            frame: None,
            engine: None,
            duration_ms: None,
            created_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };

        self.remember(artifact.clone());
        Ok(artifact)
    }

    fn remember(&self, artifact: Artifact) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        entries.push(artifact);
        let capacity = self.capacity;
        if entries.len() > capacity {
            let excess = entries.len() - capacity;
            entries.drain(0..excess);
        }
    }

    /// Look an artifact up by id.
    pub fn get(&self, id: ArtifactId) -> Option<Artifact> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|a| a.artifact_id == id)
            .cloned()
    }

    /// Most recent artifacts first.
    pub fn recent(&self, limit: usize) -> Vec<Artifact> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Turn a caller-supplied base name into a safe filename stem.
///
/// Already validated by `check_artifact_name`; this collapses the remaining
/// cosmetic problems (spaces, runs of dots) so the result reads well on disk.
fn sanitise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_separator = false;
    for ch in name.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '-' {
            last_was_separator = false;
            ch
        } else if matches!(ch, ' ' | '_' | '.') {
            if last_was_separator {
                continue;
            }
            last_was_separator = true;
            '_'
        } else {
            continue;
        };
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "output".to_string()
    } else {
        trimmed
    }
}

/// MIME type for a file extension.
pub fn mime_for(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "exr" => "image/x-exr",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "tga" => "image/x-tga",
        "fbx" => "application/octet-stream",
        "obj" => "text/plain",
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        "usd" | "usda" | "usdc" | "usdz" => "model/vnd.usd",
        "stl" => "model/stl",
        "ply" => "model/mesh",
        "dae" => "model/vnd.collada+xml",
        "abc" => "application/octet-stream",
        "blend" => "application/x-blender",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let mut config = Config::default();
        config.workspace = std::env::temp_dir().join("blender-mcp-artifact-test");
        config.project_root = config.workspace.join("project");
        config.prepare_directories().unwrap();
        config
    }

    #[test]
    fn names_are_sanitised_not_trusted() {
        assert_eq!(sanitise("hero shot"), "hero_shot");
        assert_eq!(sanitise("a...b"), "a_b");
        assert_eq!(sanitise("___"), "output");
        assert_eq!(sanitise("Turntable-01"), "Turntable-01");
    }

    #[test]
    fn allocation_stays_inside_the_root() {
        let config = config();
        let store = ArtifactStore::new(10);
        let path = store
            .allocate(&config, Root::Renders, "shot", "png")
            .unwrap();
        assert!(path.starts_with(config.root_path(Root::Renders).canonicalize().unwrap()));
        assert!(path.to_string_lossy().ends_with("shot.png"));
    }

    #[test]
    fn allocation_refuses_a_path_disguised_as_a_name() {
        let config = config();
        let store = ArtifactStore::new(10);
        for bad in ["../escape", "sub/dir", "C:/absolute"] {
            assert!(
                store.allocate(&config, Root::Renders, bad, "png").is_err(),
                "`{bad}` should have been refused"
            );
        }
    }

    #[test]
    fn allocation_does_not_overwrite() {
        let config = config();
        let store = ArtifactStore::new(10);
        let first = store
            .allocate(&config, Root::Exports, "unique_probe", "obj")
            .unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = store
            .allocate(&config, Root::Exports, "unique_probe", "obj")
            .unwrap();
        assert_ne!(first, second, "an existing file must not be reused");
        std::fs::remove_file(&first).ok();
    }

    #[test]
    fn registering_a_missing_file_is_an_error() {
        let config = config();
        let store = ArtifactStore::new(10);
        let missing = config.root_path(Root::Renders).join("never-written.png");
        let error = store
            .register(&config, Root::Renders, &missing, "image/png")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BlenderInternalError);
    }

    #[test]
    fn the_index_is_bounded_but_files_are_not_touched() {
        let config = config();
        let store = ArtifactStore::new(2);
        let mut paths = Vec::new();
        for index in 0..3 {
            let path = store
                .allocate(&config, Root::Temp, &format!("bounded{index}"), "png")
                .unwrap();
            std::fs::write(&path, b"data").unwrap();
            store
                .register(&config, Root::Temp, &path, "image/png")
                .unwrap();
            paths.push(path);
        }
        assert_eq!(store.len(), 2, "the index is capped");
        for path in &paths {
            assert!(path.exists(), "capping the index must not delete files");
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn mime_types_cover_the_formats_we_produce() {
        assert_eq!(mime_for("PNG"), "image/png");
        assert_eq!(mime_for("glb"), "model/gltf-binary");
        assert_eq!(mime_for("wat"), "application/octet-stream");
    }
}

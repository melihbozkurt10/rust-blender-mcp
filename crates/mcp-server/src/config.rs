//! Server configuration, read from the environment.
//!
//! Environment variables rather than a config file: an MCP server is launched
//! by a client that already has a place to put environment variables, and a
//! file would be one more thing to keep in sync.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use blender_protocol::command::Category;

/// Everything the server needs to start.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the Blender add-on dials into.
    pub bind: SocketAddr,
    /// Root of the managed filesystem tree.
    pub workspace: PathBuf,
    /// Project directory imports and exports are relative to.
    pub project_root: PathBuf,
    /// Register every tool at startup instead of lazily by category.
    pub eager_tools: bool,
    /// Categories enabled before any client asks.
    pub default_categories: Vec<Category>,
    /// Largest frame on the bridge socket.
    pub max_frame_bytes: u32,
    /// Deadline for an ordinary request.
    pub request_timeout: Duration,
    /// Ceiling on how many operations one batch may contain.
    pub max_batch_operations: usize,
    /// How many scene revisions to keep for diffing.
    pub revision_history: usize,
    /// Poly Haven is public and needs no credentials; Sketchfab does.
    ///
    /// Held as a `Secret` so that neither this struct's `Debug` output nor any
    /// log line that prints the configuration can expose it.
    pub sketchfab_token: Option<asset_providers::Secret>,
    /// Allow asset downloads at all.
    pub allow_asset_downloads: bool,
    /// Largest single asset download, in bytes.
    pub max_download_bytes: u64,
}

/// Where a managed subdirectory lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    Project,
    Cache,
    Downloads,
    Renders,
    Exports,
    Temp,
}

impl Root {
    pub const ALL: [Root; 6] = [
        Root::Project,
        Root::Cache,
        Root::Downloads,
        Root::Renders,
        Root::Exports,
        Root::Temp,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Root::Project => "project",
            Root::Cache => "cache",
            Root::Downloads => "downloads",
            Root::Renders => "renders",
            Root::Exports => "exports",
            Root::Temp => "temp",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Root::ALL.into_iter().find(|r| r.id() == value)
    }
}

impl Default for Config {
    fn default() -> Self {
        let workspace = default_workspace();
        Self {
            bind: SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                blender_client::DEFAULT_PORT,
            ),
            project_root: workspace.join("project"),
            workspace,
            eager_tools: false,
            default_categories: vec![Category::Core],
            max_frame_bytes: blender_client::DEFAULT_MAX_FRAME_BYTES,
            request_timeout: Duration::from_secs(15),
            max_batch_operations: 200,
            revision_history: 1000,
            sketchfab_token: None,
            allow_asset_downloads: true,
            max_download_bytes: 512 * 1024 * 1024,
        }
    }
}

fn default_workspace() -> PathBuf {
    // A stable per-user location, so artifacts survive between server restarts
    // and are not scattered through the system temp directory.
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("blender-mcp")
}

impl Config {
    /// Read configuration from the environment, falling back to defaults.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut config = Config::default();

        if let Some(value) = env("BLENDER_MCP_HOST") {
            let ip: IpAddr = value
                .parse()
                .map_err(|_| ConfigError::Invalid("BLENDER_MCP_HOST", value.clone()))?;
            config.bind.set_ip(ip);
        }
        if let Some(value) = env("BLENDER_MCP_PORT") {
            let port: u16 = value
                .parse()
                .map_err(|_| ConfigError::Invalid("BLENDER_MCP_PORT", value.clone()))?;
            config.bind.set_port(port);
        }
        if let Some(value) = env("BLENDER_MCP_WORKSPACE") {
            config.workspace = PathBuf::from(value);
            config.project_root = config.workspace.join("project");
        }
        if let Some(value) = env("BLENDER_MCP_PROJECT_ROOT") {
            config.project_root = PathBuf::from(value);
        }
        if let Some(value) = env("BLENDER_MCP_EAGER_TOOLS") {
            config.eager_tools = is_truthy(&value);
        }
        if let Some(value) = env("BLENDER_MCP_CATEGORIES") {
            let mut categories = vec![Category::Core];
            for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let category = Category::parse(part)
                    .ok_or_else(|| ConfigError::UnknownCategory(part.to_string()))?;
                if !categories.contains(&category) {
                    categories.push(category);
                }
            }
            config.default_categories = categories;
        }
        if let Some(value) = env("BLENDER_MCP_MAX_FRAME_BYTES") {
            config.max_frame_bytes = value
                .parse()
                .map_err(|_| ConfigError::Invalid("BLENDER_MCP_MAX_FRAME_BYTES", value.clone()))?;
        }
        if let Some(value) = env("BLENDER_MCP_REQUEST_TIMEOUT_SECS") {
            let seconds: u64 = value.parse().map_err(|_| {
                ConfigError::Invalid("BLENDER_MCP_REQUEST_TIMEOUT_SECS", value.clone())
            })?;
            config.request_timeout = Duration::from_secs(seconds.clamp(1, 3600));
        }
        if let Some(value) = env("BLENDER_MCP_MAX_BATCH_OPERATIONS") {
            config.max_batch_operations = value
                .parse::<usize>()
                .map_err(|_| {
                    ConfigError::Invalid("BLENDER_MCP_MAX_BATCH_OPERATIONS", value.clone())
                })?
                .clamp(1, 10_000);
        }
        if let Some(value) = env("BLENDER_MCP_REVISION_HISTORY") {
            config.revision_history = value
                .parse::<usize>()
                .map_err(|_| ConfigError::Invalid("BLENDER_MCP_REVISION_HISTORY", value.clone()))?
                .clamp(1, 100_000);
        }
        // Read but never logged, never echoed in a tool result.
        config.sketchfab_token = asset_providers::Secret::from_env("BLENDER_MCP_SKETCHFAB_TOKEN");
        if let Some(value) = env("BLENDER_MCP_ALLOW_ASSET_DOWNLOADS") {
            config.allow_asset_downloads = is_truthy(&value);
        }
        if let Some(value) = env("BLENDER_MCP_MAX_DOWNLOAD_BYTES") {
            config.max_download_bytes = value.parse().map_err(|_| {
                ConfigError::Invalid("BLENDER_MCP_MAX_DOWNLOAD_BYTES", value.clone())
            })?;
        }

        config.validate()?;
        Ok(config)
    }

    /// How the asset providers should be configured.
    ///
    /// Downloads are confined to the managed downloads root, and the size cap
    /// is the server's, not the provider's.
    pub fn asset_config(&self) -> asset_providers::Config {
        let mut config = asset_providers::Config::new(self.root_path(Root::Downloads));
        config.policy.max_bytes = self.max_download_bytes;
        // One asset may be a texture set of several files; allow a few of them
        // before refusing, but keep a ceiling.
        config.policy.max_total_bytes = self.max_download_bytes.saturating_mul(8);
        config.sketchfab_token = self.sketchfab_token.clone();
        config
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let ip = self.bind.ip();
        let loopback = match ip {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback(),
        };
        if !loopback && env("BLENDER_MCP_ALLOW_REMOTE").is_none_or(|v| !is_truthy(&v)) {
            // Binding off-loopback exposes full scene mutation to anything that
            // can reach the port. It stays possible, but only deliberately.
            return Err(ConfigError::RemoteBindRefused(self.bind));
        }
        Ok(())
    }

    /// Absolute path of a managed root.
    pub fn root_path(&self, root: Root) -> PathBuf {
        match root {
            Root::Project => self.project_root.clone(),
            other => self.workspace.join(other.id()),
        }
    }

    /// Create every managed directory.
    pub fn prepare_directories(&self) -> std::io::Result<()> {
        for root in Root::ALL {
            std::fs::create_dir_all(self.root_path(root))?;
        }
        Ok(())
    }

    /// The transport configuration derived from this one.
    pub fn transport(&self) -> blender_client::Config {
        blender_client::Config {
            bind: self.bind,
            max_frame_bytes: self.max_frame_bytes,
            request_timeout: self.request_timeout,
            client_name: "rust-blender-mcp".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            ..blender_client::Config::default()
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Why the configuration was rejected.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not a valid value: `{1}`")]
    Invalid(&'static str, String),
    #[error("`{0}` is not a known tool category")]
    UnknownCategory(String),
    #[error(
        "refusing to bind {0}: that address is reachable from other machines, which would expose \
         scene mutation to the network. Set BLENDER_MCP_ALLOW_REMOTE=1 if that is genuinely intended."
    )]
    RemoteBindRefused(SocketAddr),
    #[error("could not prepare the workspace at {0}: {1}")]
    Workspace(PathBuf, #[source] std::io::Error),
}

/// Resolve a caller-supplied relative path inside a managed root.
///
/// Two checks, because either alone is insufficient: the textual check rejects
/// the obvious traversal, and canonicalising the result catches a symlink that
/// points out of the tree. The parent directory is canonicalised rather than
/// the file, so a path that does not exist yet can still be validated.
pub fn resolve_managed_path(
    root_path: &Path,
    relative: &str,
) -> Result<PathBuf, blender_protocol::BlenderError> {
    blender_protocol::io::check_relative_path(relative)?;

    let joined = root_path.join(relative.replace('\\', "/"));
    let canonical_root = root_path
        .canonicalize()
        .unwrap_or_else(|_| root_path.to_path_buf());

    // Canonicalise whichever ancestor exists, then re-attach the rest.
    let mut existing = joined.as_path();
    let mut tail = PathBuf::new();
    loop {
        if existing.exists() {
            break;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail = if tail.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    PathBuf::from(name).join(&tail)
                };
                existing = parent;
            }
            _ => break,
        }
    }

    let resolved = match existing.canonicalize() {
        // `join("")` appends a separator, which turns a real file path into a
        // directory path that does not exist. When the whole path already
        // exists there is no tail to re-attach.
        Ok(base) if tail.as_os_str().is_empty() => base,
        Ok(base) => base.join(&tail),
        Err(_) => joined.clone(),
    };

    if !resolved.starts_with(&canonical_root) {
        return Err(blender_protocol::BlenderError::path_not_allowed(relative)
            .with_detail("root", canonical_root.display().to_string()));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use blender_protocol::ErrorCode;

    use super::*;

    #[test]
    fn roots_round_trip() {
        for root in Root::ALL {
            assert_eq!(Root::parse(root.id()), Some(root));
        }
    }

    #[test]
    fn resolving_an_existing_file_does_not_leave_a_trailing_separator() {
        // A trailing separator makes a file path name a directory, and every
        // consumer of the resolved path then reports that the file is missing.
        let temp = std::env::temp_dir().join("blender-mcp-path-existing");
        std::fs::create_dir_all(temp.join("nested")).unwrap();
        let file = temp.join("nested").join("asset.hdr");
        std::fs::write(&file, b"x").unwrap();

        let resolved = resolve_managed_path(&temp, "nested/asset.hdr").unwrap();
        assert!(resolved.is_file(), "{} is not a file", resolved.display());
        assert!(
            !resolved
                .to_string_lossy()
                .ends_with(std::path::MAIN_SEPARATOR),
            "{} ends with a separator",
            resolved.display()
        );

        // A path that does not exist yet still resolves, for writes.
        let planned = resolve_managed_path(&temp, "nested/not-yet.png").unwrap();
        assert!(planned.starts_with(temp.canonicalize().unwrap_or(temp.clone())));

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn managed_paths_stay_inside_their_root() {
        let temp = std::env::temp_dir().join("blender-mcp-path-test");
        std::fs::create_dir_all(&temp).unwrap();

        let ok = resolve_managed_path(&temp, "renders/shot.png").unwrap();
        assert!(ok.starts_with(temp.canonicalize().unwrap()));

        for bad in [
            "../escape.png",
            "/etc/passwd",
            "C:/Windows/system32",
            "a/../../b",
        ] {
            let err = resolve_managed_path(&temp, bad).unwrap_err();
            assert!(
                matches!(err.code, ErrorCode::PathNotAllowed | ErrorCode::InvalidPath),
                "`{bad}` produced {:?}",
                err.code
            );
        }
    }

    #[test]
    fn nested_new_directories_are_allowed() {
        let temp = std::env::temp_dir().join("blender-mcp-path-test-2");
        std::fs::create_dir_all(&temp).unwrap();
        let resolved = resolve_managed_path(&temp, "a/b/c/render.png").unwrap();
        assert!(
            resolved.ends_with("a/b/c/render.png") || resolved.ends_with("a\\b\\c\\render.png")
        );
    }

    #[test]
    fn defaults_are_loopback_and_lazy() {
        let config = Config::default();
        assert!(config.bind.ip().is_loopback());
        assert!(!config.eager_tools);
        assert_eq!(config.default_categories, vec![Category::Core]);
    }

    #[test]
    fn truthiness_accepts_the_usual_spellings() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(is_truthy(value), "{value}");
        }
        for value in ["0", "false", "no", "off", ""] {
            assert!(!is_truthy(value), "{value}");
        }
    }
}

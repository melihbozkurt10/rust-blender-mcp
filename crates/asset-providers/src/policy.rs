//! What this crate is willing to fetch, and what it refuses.
//!
//! Every rule here is enforced before a byte is written to disk. The URLs come
//! from a provider's API rather than from a caller, but that is not a reason to
//! trust them: a compromised or merely buggy provider response must not be able
//! to turn this process into an HTTP client for the local network, or drop an
//! executable into a directory the user later opens.
//!
//! Nothing downloaded is ever executed, unpacked into an arbitrary location, or
//! installed as a Blender add-on. Files land in the managed downloads root as
//! data, and are imported through the normal typed import path.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
};

use blender_protocol::{BlenderError, ErrorCode, Result};
use url::{Host, Url};

/// File extensions this crate will write to disk.
///
/// An allowlist rather than a denylist: a new dangerous extension appearing in
/// the world must not silently become downloadable, and the set of things a 3D
/// asset can legitimately be is small and stable.
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    // images and texture maps
    "png", "jpg", "jpeg", "webp", "tif", "tiff", "tga", "exr", "hdr", // geometry
    "blend", "gltf", "glb", "fbx", "obj", "mtl", "ply", "stl", "abc", "usd", "usda", "usdc",
    "usdz", // containers
    "zip",
];

/// Content types a download may claim to be.
///
/// Matched on prefix, because servers append parameters (`image/png;
/// charset=binary`). `application/octet-stream` is here because most CDNs serve
/// every binary that way; the extension allowlist is what actually constrains
/// the file.
pub const ALLOWED_CONTENT_TYPES: &[&str] = &[
    "image/",
    "model/",
    "application/octet-stream",
    "application/zip",
    "application/x-zip-compressed",
    "binary/octet-stream",
];

/// Limits applied to every fetch.
#[derive(Debug, Clone)]
pub struct DownloadPolicy {
    /// Largest single file, in bytes. Enforced against the declared length
    /// *and* against the bytes actually received, because a server is free to
    /// lie about `Content-Length` or omit it.
    pub max_bytes: u64,
    /// Largest total for one asset across all its files.
    pub max_total_bytes: u64,
    /// How many redirects to follow. Each hop is re-checked against the same
    /// URL rules, so a redirect cannot be used to reach a blocked host.
    pub max_redirects: usize,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    /// Whether to allow hosts that resolve to loopback or private addresses.
    /// Off by default; only useful when pointing a test at a local server.
    pub allow_private_hosts: bool,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            max_redirects: 5,
            timeout_secs: 300,
            allow_private_hosts: false,
        }
    }
}

impl DownloadPolicy {
    /// Parse and vet a URL.
    pub fn check_url(&self, url: &str) -> Result<Url> {
        let parsed = Url::parse(url).map_err(|error| {
            BlenderError::new(
                ErrorCode::AssetProviderError,
                format!("The provider returned a URL that cannot be parsed: {error}."),
            )
        })?;

        if parsed.scheme() != "https" {
            return Err(BlenderError::new(
                ErrorCode::AssetProviderError,
                format!(
                    "Refusing to fetch over `{}`; asset downloads must use HTTPS.",
                    parsed.scheme()
                ),
            )
            .with_detail("scheme", parsed.scheme()));
        }

        // Credentials embedded in a URL would be sent to whatever host the URL
        // names, and would end up in any log that records the URL.
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(BlenderError::new(
                ErrorCode::AssetProviderError,
                "Refusing to fetch a URL with embedded credentials.",
            ));
        }

        match parsed.host() {
            None => {
                return Err(BlenderError::new(
                    ErrorCode::AssetProviderError,
                    "Refusing to fetch a URL with no host.",
                ));
            }
            Some(host) if !self.allow_private_hosts => {
                if let Some(reason) = private_host_reason(&host) {
                    return Err(BlenderError::new(
                        ErrorCode::AssetProviderError,
                        format!("Refusing to fetch from {host}: {reason}."),
                    )
                    .with_detail("host", host.to_string()));
                }
            }
            Some(_) => {}
        }

        Ok(parsed)
    }

    /// Turn a provider-supplied name into something safe to write.
    ///
    /// Path separators, traversal and control characters are rejected outright
    /// rather than sanitised away, because a name that needs sanitising is a
    /// sign the provider response is not what was expected.
    pub fn safe_filename(&self, candidate: &str) -> Result<String> {
        let name = candidate.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(invalid_filename(
                candidate,
                "it must be 1 to 128 characters",
            ));
        }
        if name.starts_with('.') {
            return Err(invalid_filename(candidate, "it must not start with a dot"));
        }
        if name.contains(['/', '\\', '\0', ':']) || name.contains("..") {
            return Err(invalid_filename(
                candidate,
                "it must not contain path separators, `:` or `..`",
            ));
        }
        if name
            .chars()
            .any(|c| c.is_control() || !(c.is_ascii_alphanumeric() || "._- ".contains(c)))
        {
            return Err(invalid_filename(
                candidate,
                "it must use only letters, digits, dots, dashes, underscores and spaces",
            ));
        }
        self.check_extension(name)?;
        Ok(name.to_string())
    }

    /// Vet a relative path a provider wants to write inside the asset's own
    /// directory.
    ///
    /// Model archives reference their textures by relative path
    /// (`textures/rock_diff.jpg`), and flattening those would break the file
    /// that points at them. Each component is validated exactly as a filename
    /// is, and the depth is bounded.
    pub fn safe_relative_path(&self, candidate: &str) -> Result<Vec<String>> {
        let normalised = candidate.trim().replace('\\', "/");
        // An absolute path is not a relative path with a stray separator; it is
        // a different intent, and the only safe answer to it is no.
        if normalised.starts_with('/') || normalised.contains(':') {
            return Err(invalid_filename(
                candidate,
                "it must be relative to the asset's own directory",
            ));
        }
        let components: Vec<&str> = normalised.split('/').filter(|c| !c.is_empty()).collect();
        if components.is_empty() || components.len() > 4 {
            return Err(invalid_filename(
                candidate,
                "it must have between one and four path components",
            ));
        }
        let (last, directories) = components.split_last().expect("checked non-empty");
        let mut parts = Vec::with_capacity(components.len());
        for directory in directories {
            parts.push(self.safe_directory(directory, candidate)?);
        }
        parts.push(self.safe_filename(last)?);
        Ok(parts)
    }

    fn safe_directory(&self, component: &str, candidate: &str) -> Result<String> {
        if component.is_empty()
            || component.len() > 64
            || component.starts_with('.')
            || component.contains("..")
            || !component
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._- ".contains(c))
        {
            return Err(invalid_filename(
                candidate,
                "each directory component must be a plain name",
            ));
        }
        Ok(component.to_string())
    }

    /// Check a filename's extension against the allowlist.
    pub fn check_extension(&self, filename: &str) -> Result<()> {
        let extension = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);

        match extension {
            Some(extension) if ALLOWED_EXTENSIONS.contains(&extension.as_str()) => Ok(()),
            Some(extension) => Err(BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                format!(
                    "`.{extension}` is not a downloadable asset format. Only asset data is \
                     fetched, and nothing downloaded is ever executed or installed."
                ),
            )
            .with_detail("extension", extension)
            .with_detail_json("allowed_extensions", &ALLOWED_EXTENSIONS)),
            None => Err(BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                format!("`{filename}` has no file extension, so its format cannot be checked."),
            )),
        }
    }

    pub fn check_content_type(&self, content_type: &str) -> Result<()> {
        let value = content_type.trim().to_ascii_lowercase();
        if ALLOWED_CONTENT_TYPES
            .iter()
            .any(|allowed| value.starts_with(allowed))
        {
            return Ok(());
        }
        Err(BlenderError::new(
            ErrorCode::AssetDownloadFailed,
            format!("The server offered `{content_type}`, which is not asset data."),
        )
        .with_detail("content_type", content_type))
    }

    pub fn check_size(&self, bytes: u64) -> Result<()> {
        if bytes > self.max_bytes {
            return Err(BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                format!(
                    "The file is {bytes} bytes, over the {} byte limit. Ask for a smaller \
                     resolution or raise BLENDER_MCP_MAX_DOWNLOAD_BYTES.",
                    self.max_bytes
                ),
            )
            .with_detail("size_bytes", bytes)
            .with_detail("max_bytes", self.max_bytes));
        }
        Ok(())
    }

    pub fn check_total_size(&self, bytes: u64) -> Result<()> {
        if bytes > self.max_total_bytes {
            return Err(BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                format!(
                    "The download totals {bytes} bytes, over the {} byte limit for one asset.",
                    self.max_total_bytes
                ),
            )
            .with_detail("total_bytes", bytes)
            .with_detail("max_total_bytes", self.max_total_bytes));
        }
        Ok(())
    }
}

fn invalid_filename(candidate: &str, reason: &str) -> BlenderError {
    BlenderError::new(
        ErrorCode::AssetProviderError,
        format!("The provider offered the filename `{candidate}`, but {reason}."),
    )
    .with_detail("filename", candidate)
}

/// Why a host must not be fetched from, if it must not be.
///
/// Only literal addresses can be judged here; a name that *resolves* to a
/// private address is caught by the connector at connect time. Blocking the
/// literals shuts the obvious door, which is the one a malformed provider
/// response would walk through.
fn private_host_reason(host: &Host<&str>) -> Option<&'static str> {
    match host {
        Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if name == "localhost" || name.ends_with(".localhost") {
                Some("it is the local machine")
            } else if name.ends_with(".local") || name.ends_with(".internal") {
                Some("it is a local network name")
            } else if !name.contains('.') {
                Some("a bare hostname names something on the local network")
            } else {
                None
            }
        }
        Host::Ipv4(address) => ipv4_reason(*address),
        Host::Ipv6(address) => ipv6_reason(*address),
    }
}

fn ipv4_reason(address: Ipv4Addr) -> Option<&'static str> {
    if address.is_loopback() {
        Some("it is the local machine")
    } else if address.is_private() {
        Some("it is a private network address")
    } else if address.is_link_local() {
        // 169.254.169.254 is the cloud metadata endpoint; the whole range goes.
        Some("it is a link-local address")
    } else if address.is_unspecified() || address.is_broadcast() || address.is_multicast() {
        Some("it is not a routable host address")
    } else if address.octets()[0] == 100 && (64..128).contains(&address.octets()[1]) {
        Some("it is a carrier-grade NAT address")
    } else {
        None
    }
}

fn ipv6_reason(address: Ipv6Addr) -> Option<&'static str> {
    if address.is_loopback() {
        Some("it is the local machine")
    } else if address.is_unspecified() || address.is_multicast() {
        Some("it is not a routable host address")
    } else if let Some(mapped) = address.to_ipv4_mapped() {
        // ::ffff:127.0.0.1 is loopback wearing a hat.
        ipv4_reason(mapped)
    } else {
        let segments = address.segments();
        // fc00::/7 unique-local, fe80::/10 link-local.
        if segments[0] & 0xfe00 == 0xfc00 {
            Some("it is a unique-local address")
        } else if segments[0] & 0xffc0 == 0xfe80 {
            Some("it is a link-local address")
        } else {
            None
        }
    }
}

/// The address of a resolved host, for the connect-time check.
pub fn is_blocked_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_reason(address).is_some(),
        IpAddr::V6(address) => ipv6_reason(address).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DownloadPolicy {
        DownloadPolicy::default()
    }

    #[test]
    fn plain_http_is_refused() {
        let error = policy()
            .check_url("http://dl.polyhaven.org/file.exr")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetProviderError);
        assert!(policy().check_url("https://dl.polyhaven.org/f.exr").is_ok());
    }

    #[test]
    fn non_http_schemes_are_refused() {
        for url in ["file:///etc/passwd", "ftp://example.com/a.zip", "data:,hi"] {
            assert!(policy().check_url(url).is_err(), "{url} was allowed");
        }
    }

    #[test]
    fn the_local_network_is_out_of_reach() {
        for url in [
            "https://localhost/a.png",
            "https://127.0.0.1/a.png",
            "https://[::1]/a.png",
            "https://10.0.0.5/a.png",
            "https://192.168.1.1/a.png",
            "https://172.16.4.4/a.png",
            "https://169.254.169.254/latest/meta-data/",
            "https://[fd00::1]/a.png",
            "https://[::ffff:127.0.0.1]/a.png",
            "https://intranet/a.png",
            "https://printer.local/a.png",
        ] {
            let error = policy().check_url(url).unwrap_err();
            assert_eq!(error.code, ErrorCode::AssetProviderError, "{url}");
        }
    }

    #[test]
    fn a_local_server_can_be_allowed_deliberately() {
        let policy = DownloadPolicy {
            allow_private_hosts: true,
            ..DownloadPolicy::default()
        };
        assert!(policy.check_url("https://127.0.0.1:8443/a.png").is_ok());
    }

    #[test]
    fn embedded_credentials_are_refused() {
        assert!(
            policy()
                .check_url("https://user:pass@example.com/a.png")
                .is_err()
        );
    }

    #[test]
    fn executables_and_scripts_are_not_asset_formats() {
        for name in [
            "install.py",
            "setup.exe",
            "lib.dll",
            "run.sh",
            "go.bat",
            "addon.pyc",
            "thing.so",
        ] {
            let error = policy().check_extension(name).unwrap_err();
            assert_eq!(error.code, ErrorCode::AssetDownloadFailed, "{name}");
        }
        for name in ["rock_diff_4k.jpg", "studio.exr", "chair.glb", "set.zip"] {
            assert!(policy().check_extension(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn filenames_cannot_escape_their_directory() {
        for name in [
            "../../etc/passwd",
            "a/b.png",
            "a\\b.png",
            "C:evil.png",
            ".hidden.png",
            "",
            "  ",
        ] {
            assert!(
                policy().safe_filename(name).is_err(),
                "{name:?} was allowed"
            );
        }
        assert_eq!(
            policy().safe_filename(" rocky_terrain_4k.exr ").unwrap(),
            "rocky_terrain_4k.exr",
            "surrounding whitespace is trimmed, not rejected"
        );
    }

    #[test]
    fn a_relative_path_keeps_its_directories_but_cannot_climb() {
        let policy = policy();
        assert_eq!(
            policy
                .safe_relative_path("textures/rock_diff_2k.jpg")
                .unwrap(),
            vec!["textures".to_string(), "rock_diff_2k.jpg".to_string()]
        );
        assert_eq!(
            policy.safe_relative_path("a\\b\\c.png").unwrap().len(),
            3,
            "backslashes are path separators too"
        );
        for path in [
            "../secrets/key.png",
            "textures/../../key.png",
            "a/b/c/d/e/f.png",
            "textures/setup.py",
            "/etc/passwd.png",
            ".git/config.png",
        ] {
            assert!(
                policy.safe_relative_path(path).is_err(),
                "{path} was allowed"
            );
        }
    }

    #[test]
    fn content_types_are_checked_by_prefix() {
        assert!(
            policy()
                .check_content_type("image/png; charset=binary")
                .is_ok()
        );
        assert!(
            policy()
                .check_content_type("application/octet-stream")
                .is_ok()
        );
        let error = policy().check_content_type("text/html").unwrap_err();
        assert_eq!(error.code, ErrorCode::AssetDownloadFailed);
    }

    #[test]
    fn size_limits_report_what_was_exceeded() {
        let policy = DownloadPolicy {
            max_bytes: 100,
            max_total_bytes: 150,
            ..DownloadPolicy::default()
        };
        assert!(policy.check_size(100).is_ok());
        let error = policy.check_size(101).unwrap_err();
        assert_eq!(error.details["max_bytes"], 100);
        assert!(policy.check_total_size(200).is_err());
    }
}

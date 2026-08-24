//! Fetching, behind a trait so providers can be tested without a network.
//!
//! Nothing else in this crate touches `reqwest`. A provider builds URLs and
//! parses JSON; a [`Fetcher`] performs the request and enforces the transfer
//! rules from [`crate::policy`]. In tests the fetcher is a map from URL to
//! canned response, which is what makes the provider parsing testable at all.

use std::{future::Future, path::Path, pin::Pin, sync::Arc, time::Duration};

use blender_protocol::{BlenderError, ErrorCode, Result};
use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{credentials::Secret, policy::DownloadPolicy};

/// A boxed future, so [`Fetcher`] stays object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How to authenticate one request.
///
/// Holds a [`Secret`], so it cannot be printed by accident.
#[derive(Debug, Clone)]
pub struct Authorization {
    /// The scheme word that precedes the token, e.g. `Token` or `Bearer`.
    pub scheme: &'static str,
    pub token: Secret,
}

impl Authorization {
    pub fn token(token: Secret) -> Self {
        Self {
            scheme: "Token",
            token,
        }
    }

    pub fn bearer(token: Secret) -> Self {
        Self {
            scheme: "Bearer",
            token,
        }
    }

    fn header_value(&self) -> String {
        format!("{} {}", self.scheme, self.token.expose())
    }
}

/// A file that has been written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub size_bytes: u64,
    pub sha256: String,
    pub mime_type: Option<String>,
}

/// Something that can perform HTTP GETs.
pub trait Fetcher: Send + Sync {
    /// Fetch JSON from an API endpoint.
    fn get_json<'a>(
        &'a self,
        url: &'a str,
        auth: Option<&'a Authorization>,
    ) -> BoxFuture<'a, Result<Value>>;

    /// Fetch a file to `destination`, enforcing the policy while streaming.
    ///
    /// The destination's parent directory must already exist. The file is
    /// written to a temporary name and moved into place only once complete, so
    /// an interrupted download never leaves something that looks cached.
    fn download<'a>(
        &'a self,
        url: &'a str,
        auth: Option<&'a Authorization>,
        destination: &'a Path,
    ) -> BoxFuture<'a, Result<Fetched>>;
}

/// The real fetcher.
pub struct HttpFetcher {
    client: reqwest::Client,
    policy: Arc<DownloadPolicy>,
}

/// JSON responses are metadata; anything this size is a bug or an attack, not a
/// search result.
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;

impl HttpFetcher {
    pub fn new(policy: Arc<DownloadPolicy>) -> Result<Self> {
        let redirect_policy = {
            let policy = Arc::clone(&policy);
            reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= policy.max_redirects {
                    return attempt.error(format!("more than {} redirects", policy.max_redirects));
                }
                // Every hop is re-checked: a redirect must not be a way to
                // reach a host the first URL could not have named.
                match policy.check_url(attempt.url().as_str()) {
                    Ok(_) => attempt.follow(),
                    Err(_) => attempt.stop(),
                }
            })
        };

        let client = reqwest::Client::builder()
            .user_agent(concat!("blender-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(policy.timeout_secs))
            .connect_timeout(Duration::from_secs(15))
            .redirect(redirect_policy)
            .https_only(true)
            .build()
            .map_err(|error| {
                BlenderError::internal(format!("Could not build the HTTP client: {error}."))
            })?;

        Ok(Self { client, policy })
    }

    pub fn policy(&self) -> &DownloadPolicy {
        &self.policy
    }

    fn request(&self, url: &url::Url, auth: Option<&Authorization>) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url.clone());
        if let Some(auth) = auth {
            request = request.header(reqwest::header::AUTHORIZATION, auth.header_value());
        }
        request
    }
}

impl Fetcher for HttpFetcher {
    fn get_json<'a>(
        &'a self,
        url: &'a str,
        auth: Option<&'a Authorization>,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let parsed = self.policy.check_url(url)?;
            let response = self
                .request(&parsed, auth)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|error| transport_error(url, &error))?;

            let response = check_status(url, response)?;

            if let Some(length) = response.content_length()
                && length > MAX_JSON_BYTES
            {
                return Err(BlenderError::new(
                    ErrorCode::AssetProviderError,
                    format!("The provider returned {length} bytes of JSON, which is not credible."),
                ));
            }

            let body = response
                .bytes()
                .await
                .map_err(|error| transport_error(url, &error))?;
            if body.len() as u64 > MAX_JSON_BYTES {
                return Err(BlenderError::new(
                    ErrorCode::AssetProviderError,
                    "The provider returned an implausibly large JSON response.",
                ));
            }

            serde_json::from_slice(&body).map_err(|error| {
                BlenderError::new(
                    ErrorCode::AssetProviderError,
                    format!("The provider's response was not valid JSON: {error}."),
                )
            })
        })
    }

    fn download<'a>(
        &'a self,
        url: &'a str,
        auth: Option<&'a Authorization>,
        destination: &'a Path,
    ) -> BoxFuture<'a, Result<Fetched>> {
        Box::pin(async move {
            let parsed = self.policy.check_url(url)?;
            let response = self
                .request(&parsed, auth)
                .send()
                .await
                .map_err(|error| transport_error(url, &error))?;
            let response = check_status(url, response)?;

            if let Some(content_type) = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
            {
                self.policy.check_content_type(content_type)?;
            }
            let mime_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());

            // A declared length over the limit saves downloading it to find out.
            if let Some(length) = response.content_length() {
                self.policy.check_size(length)?;
            }

            let partial = partial_path(destination);
            let mut file = tokio::fs::File::create(&partial)
                .await
                .map_err(|error| write_error(&partial, &error))?;

            let mut hasher = Sha256::new();
            let mut received: u64 = 0;
            let mut stream = response.bytes_stream();

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = tokio::fs::remove_file(&partial).await;
                        return Err(transport_error(url, &error));
                    }
                };
                received += chunk.len() as u64;
                // Checked while streaming, because a server that omits or
                // understates Content-Length would otherwise fill the disk.
                if let Err(error) = self.policy.check_size(received) {
                    drop(file);
                    let _ = tokio::fs::remove_file(&partial).await;
                    return Err(error);
                }
                hasher.update(&chunk);
                if let Err(error) = file.write_all(&chunk).await {
                    let _ = tokio::fs::remove_file(&partial).await;
                    return Err(write_error(&partial, &error));
                }
            }

            file.flush()
                .await
                .map_err(|error| write_error(&partial, &error))?;
            drop(file);

            tokio::fs::rename(&partial, destination)
                .await
                .map_err(|error| write_error(destination, &error))?;

            Ok(Fetched {
                size_bytes: received,
                sha256: format!("{:x}", hasher.finalize()),
                mime_type,
            })
        })
    }
}

fn partial_path(destination: &Path) -> std::path::PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".part");
    std::path::PathBuf::from(name)
}

fn check_status(url: &str, response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let (code, message) = match status.as_u16() {
        401 | 403 => (
            ErrorCode::AssetAuthRequired,
            "The provider rejected the credentials, or this asset needs an account.".to_string(),
        ),
        404 | 410 => (
            ErrorCode::AssetNotFound,
            "The provider does not have this asset.".to_string(),
        ),
        429 => (
            ErrorCode::RateLimited,
            "The provider is rate limiting this server. Wait and try again.".to_string(),
        ),
        _ => (
            ErrorCode::AssetProviderError,
            format!("The provider answered with HTTP {status}."),
        ),
    };

    // The URL goes into details rather than the message, and never the token:
    // a query string can contain a signed parameter worth keeping out of prose.
    Err(BlenderError::new(code, message)
        .with_detail("status", status.as_u16())
        .with_detail("url", redact(url)))
}

fn transport_error(url: &str, error: &reqwest::Error) -> BlenderError {
    let code = if error.is_timeout() {
        ErrorCode::Timeout
    } else {
        ErrorCode::AssetDownloadFailed
    };
    BlenderError::new(
        code,
        format!(
            "The request to the provider failed: {}.",
            error_summary(error)
        ),
    )
    .with_detail("url", redact(url))
}

/// `reqwest`'s `Display` includes the URL, which can carry a signed query
/// parameter. Only the cause is kept.
fn error_summary(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "the request timed out".to_string()
    } else if error.is_connect() {
        "the connection could not be established".to_string()
    } else if error.is_redirect() {
        "too many redirects, or a redirect to a host that is not allowed".to_string()
    } else if error.is_body() || error.is_decode() {
        "the response body could not be read".to_string()
    } else {
        "the request could not be completed".to_string()
    }
}

/// Keep the origin and path, drop the query string.
fn redact(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            let _ = parsed.set_password(None);
            let _ = parsed.set_username("");
            parsed.to_string()
        }
        Err(_) => "<unparseable url>".to_string(),
    }
}

fn write_error(path: &Path, error: &std::io::Error) -> BlenderError {
    BlenderError::new(
        ErrorCode::AssetDownloadFailed,
        format!("Could not write the downloaded file: {error}."),
    )
    .with_detail("path", path.display().to_string())
}

/// A fetcher backed by canned responses, for tests.
#[cfg(any(test, feature = "testing"))]
pub mod stub {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;

    /// What the stub should answer for one URL.
    #[derive(Clone)]
    pub enum Canned {
        Json(Value),
        Bytes(Vec<u8>),
        Fail(ErrorCode, String),
    }

    /// A [`Fetcher`] that answers from a map and records what was asked for.
    #[derive(Default)]
    pub struct StubFetcher {
        responses: Mutex<HashMap<String, Canned>>,
        requests: Mutex<Vec<String>>,
        /// URLs that were sent an Authorization header.
        authorized: Mutex<Vec<String>>,
    }

    impl StubFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with(self, url: &str, canned: Canned) -> Self {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_string(), canned);
            self
        }

        pub fn json(self, url: &str, value: Value) -> Self {
            self.with(url, Canned::Json(value))
        }

        pub fn bytes(self, url: &str, bytes: impl Into<Vec<u8>>) -> Self {
            self.with(url, Canned::Bytes(bytes.into()))
        }

        /// Every URL requested, in order.
        pub fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        pub fn authorized(&self) -> Vec<String> {
            self.authorized.lock().unwrap().clone()
        }

        fn answer(&self, url: &str, auth: Option<&Authorization>) -> Result<Canned> {
            self.requests.lock().unwrap().push(url.to_string());
            if auth.is_some() {
                self.authorized.lock().unwrap().push(url.to_string());
            }
            self.responses
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| {
                    BlenderError::new(
                        ErrorCode::AssetNotFound,
                        format!("The stub has no response for `{url}`."),
                    )
                })
        }
    }

    impl Fetcher for StubFetcher {
        fn get_json<'a>(
            &'a self,
            url: &'a str,
            auth: Option<&'a Authorization>,
        ) -> BoxFuture<'a, Result<Value>> {
            Box::pin(async move {
                match self.answer(url, auth)? {
                    Canned::Json(value) => Ok(value),
                    Canned::Bytes(_) => Err(BlenderError::new(
                        ErrorCode::AssetProviderError,
                        "The stub holds bytes, not JSON, for this URL.",
                    )),
                    Canned::Fail(code, message) => Err(BlenderError::new(code, message)),
                }
            })
        }

        fn download<'a>(
            &'a self,
            url: &'a str,
            auth: Option<&'a Authorization>,
            destination: &'a Path,
        ) -> BoxFuture<'a, Result<Fetched>> {
            Box::pin(async move {
                let bytes = match self.answer(url, auth)? {
                    Canned::Bytes(bytes) => bytes,
                    Canned::Json(value) => serde_json::to_vec(&value).unwrap_or_default(),
                    Canned::Fail(code, message) => {
                        return Err(BlenderError::new(code, message));
                    }
                };
                tokio::fs::write(destination, &bytes)
                    .await
                    .map_err(|error| write_error(destination, &error))?;
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                Ok(Fetched {
                    size_bytes: bytes.len() as u64,
                    sha256: format!("{:x}", hasher.finalize()),
                    mime_type: Some("application/octet-stream".to_string()),
                })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_string_never_reaches_an_error_message() {
        let redacted = redact("https://cdn.example.com/f.exr?X-Amz-Signature=deadbeef&t=1");
        assert_eq!(redacted, "https://cdn.example.com/f.exr");
        assert!(!redacted.contains("deadbeef"));
    }

    #[test]
    fn an_authorization_header_is_built_but_not_printed() {
        let auth = Authorization::token(Secret::new("abcdefghijklmnop"));
        assert_eq!(auth.header_value(), "Token abcdefghijklmnop");
        assert!(!format!("{auth:?}").contains("abcdefgh"));
    }

    #[test]
    fn the_partial_name_sits_beside_the_target() {
        let partial = partial_path(Path::new("/downloads/a/rock_4k.exr"));
        assert!(partial.to_string_lossy().ends_with("rock_4k.exr.part"));
    }

    #[tokio::test]
    async fn the_stub_records_what_was_asked_for() {
        use stub::{Canned, StubFetcher};

        let fetcher = StubFetcher::new()
            .json("https://api.example.com/a", serde_json::json!({"ok": true}))
            .with(
                "https://api.example.com/b",
                Canned::Fail(ErrorCode::AssetNotFound, "gone".into()),
            );

        assert_eq!(
            fetcher
                .get_json("https://api.example.com/a", None)
                .await
                .unwrap()["ok"],
            true
        );
        assert_eq!(
            fetcher
                .get_json("https://api.example.com/b", None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::AssetNotFound
        );
        assert_eq!(fetcher.requests().len(), 2);
    }
}

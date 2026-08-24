//! API credentials.
//!
//! A token reaches this process from the environment and must never leave it:
//! not in a tool result, not in a log line, not in an error message. The type
//! here exists so that is the default rather than something to remember.

use std::{env, fmt};

/// An API token.
///
/// `Debug` and `Display` print a redaction marker, so a token cannot leak
/// through `tracing::debug!("{secret:?}")`, through a `format!` into an error
/// message, or through a struct that derives `Debug` and happens to hold one.
/// The value is only reachable through [`Secret::expose`], which is deliberately
/// awkward to type and easy to grep for.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    value: String,
}

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// Read a token from an environment variable.
    ///
    /// Whitespace is trimmed, because a token pasted into a shell profile
    /// routinely arrives with a trailing newline, and an empty variable is
    /// treated as absent rather than as an empty token.
    pub fn from_env(variable: &str) -> Option<Self> {
        let value = env::var(variable).ok()?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self::new(trimmed))
    }

    /// The token itself. Only for putting into an outgoing request header.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// A hint a human can use to tell two tokens apart in a diagnostic, without
    /// revealing enough to use either. Short tokens reveal nothing at all.
    pub fn fingerprint(&self) -> String {
        let visible: String = self.value.chars().take(4).collect();
        if self.value.chars().count() < 12 {
            return "****".to_string();
        }
        format!("{visible}\u{2026}")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_itself() {
        let secret = Secret::new("sk-live-abcdefghijklmnop");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert!(!format!("{secret:?} {secret}").contains("abcdefg"));
    }

    #[test]
    fn a_secret_inside_a_derived_debug_stays_redacted() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            token: Secret,
        }
        let printed = format!(
            "{:?}",
            Config {
                token: Secret::new("sk-live-abcdefghijklmnop")
            }
        );
        assert!(!printed.contains("abcdefg"), "got {printed}");
    }

    #[test]
    fn a_fingerprint_identifies_without_revealing() {
        let secret = Secret::new("abcdefghijklmnopqrstuvwxyz");
        assert_eq!(secret.fingerprint(), "abcd\u{2026}");
        // A token short enough that four characters would be a large fraction
        // of it reveals nothing at all.
        assert_eq!(Secret::new("abcdefg").fingerprint(), "****");
    }

    #[test]
    fn an_empty_environment_variable_is_not_a_token() {
        // The crate denies unsafe everywhere; this is the one exception, and it
        // has to be spelled out here so a second one cannot appear quietly.
        // SAFETY: single-threaded test, and the variable is ours.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var("BLENDER_MCP_TEST_TOKEN_EMPTY", "   ");
            env::set_var("BLENDER_MCP_TEST_TOKEN_SET", " abc123 ");
        }
        assert!(Secret::from_env("BLENDER_MCP_TEST_TOKEN_EMPTY").is_none());
        assert!(Secret::from_env("BLENDER_MCP_TEST_TOKEN_MISSING").is_none());
        assert_eq!(
            Secret::from_env("BLENDER_MCP_TEST_TOKEN_SET")
                .unwrap()
                .expose(),
            "abc123",
            "surrounding whitespace is not part of the token"
        );
    }
}

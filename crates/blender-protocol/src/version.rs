//! Protocol and Blender version handling.
//!
//! The wire protocol version is deliberately independent of the project
//! version: the add-on and the server ship separately and a user will run
//! mismatched builds sooner or later.

use std::{cmp::Ordering, fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of the framing + envelope contract in this crate.
///
/// Bump when an existing message shape changes meaning. Adding a new optional
/// field, or a new `op`, does not require a bump -- capability negotiation
/// covers that.
pub const PROTOCOL_VERSION: u32 = 1;

/// Oldest protocol version this build can still talk to.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// The oldest Blender release the add-on is tested against.
pub const MIN_BLENDER_VERSION: BlenderVersion = BlenderVersion::new(4, 2, 0);

/// A `major.minor.patch` Blender version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlenderVersion {
    pub major: u32,
    pub minor: u32,
    #[serde(default)]
    pub patch: u32,
}

impl BlenderVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// `true` when this version is at least `other`.
    pub fn at_least(self, other: Self) -> bool {
        self >= other
    }

    pub fn is_supported(self) -> bool {
        self.at_least(MIN_BLENDER_VERSION)
    }
}

impl PartialOrd for BlenderVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BlenderVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl fmt::Display for BlenderVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for BlenderVersion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split(['.', '-']);
        let mut next = |what: &str| -> Result<u32, String> {
            parts
                .next()
                .ok_or_else(|| format!("missing {what} component in version `{s}`"))?
                .parse::<u32>()
                .map_err(|e| format!("bad {what} component in version `{s}`: {e}"))
        };
        let major = next("major")?;
        let minor = next("minor")?;
        // Blender frequently reports two-component versions ("4.2"); treat the
        // patch as zero rather than rejecting the string.
        let patch = parts
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        Ok(Self::new(major, minor, patch))
    }
}

/// Why a handshake was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRejection {
    ProtocolTooOld {
        theirs: u32,
        minimum: u32,
    },
    ProtocolTooNew {
        theirs: u32,
        ours: u32,
    },
    BlenderTooOld {
        theirs: BlenderVersion,
        minimum: BlenderVersion,
    },
}

impl fmt::Display for VersionRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionRejection::ProtocolTooOld { theirs, minimum } => write!(
                f,
                "add-on speaks protocol v{theirs}, this server needs at least v{minimum}. Update the Blender add-on."
            ),
            VersionRejection::ProtocolTooNew { theirs, ours } => write!(
                f,
                "add-on speaks protocol v{theirs}, this server only implements v{ours}. Update the MCP server."
            ),
            VersionRejection::BlenderTooOld { theirs, minimum } => write!(
                f,
                "Blender {theirs} is older than the minimum supported {minimum}."
            ),
        }
    }
}

/// Check a peer's advertised versions against what this build supports.
pub fn check_compatibility(
    peer_protocol: u32,
    blender: BlenderVersion,
) -> Result<(), VersionRejection> {
    if peer_protocol < MIN_SUPPORTED_PROTOCOL_VERSION {
        return Err(VersionRejection::ProtocolTooOld {
            theirs: peer_protocol,
            minimum: MIN_SUPPORTED_PROTOCOL_VERSION,
        });
    }
    if peer_protocol > PROTOCOL_VERSION {
        return Err(VersionRejection::ProtocolTooNew {
            theirs: peer_protocol,
            ours: PROTOCOL_VERSION,
        });
    }
    if !blender.is_supported() {
        return Err(VersionRejection::BlenderTooOld {
            theirs: blender,
            minimum: MIN_BLENDER_VERSION,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_and_three_component_versions() {
        assert_eq!(
            "4.2".parse::<BlenderVersion>().unwrap(),
            BlenderVersion::new(4, 2, 0)
        );
        assert_eq!(
            "5.1.3".parse::<BlenderVersion>().unwrap(),
            BlenderVersion::new(5, 1, 3)
        );
        assert!("banana".parse::<BlenderVersion>().is_err());
    }

    #[test]
    fn orders_versions_numerically_not_lexically() {
        assert!(BlenderVersion::new(4, 10, 0) > BlenderVersion::new(4, 9, 9));
        assert!(BlenderVersion::new(5, 0, 0) > BlenderVersion::new(4, 99, 0));
    }

    #[test]
    fn rejects_old_blender_and_mismatched_protocols() {
        assert!(check_compatibility(PROTOCOL_VERSION, BlenderVersion::new(5, 1, 0)).is_ok());
        assert!(matches!(
            check_compatibility(PROTOCOL_VERSION, BlenderVersion::new(3, 6, 0)),
            Err(VersionRejection::BlenderTooOld { .. })
        ));
        assert!(matches!(
            check_compatibility(PROTOCOL_VERSION + 1, BlenderVersion::new(5, 1, 0)),
            Err(VersionRejection::ProtocolTooNew { .. })
        ));
    }
}

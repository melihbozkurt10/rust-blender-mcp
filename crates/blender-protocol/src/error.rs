//! The structured error taxonomy.
//!
//! Every failure that crosses the wire, and every failure the MCP layer reports
//! to a model, is one of these codes plus a machine-readable `details` object.
//! Callers must never have to parse prose to find out what went wrong, and the
//! details are chosen so a model can self-correct on the next call (an unknown
//! socket name comes back with the list of sockets that *do* exist).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Stable error codes. New variants may be added; clients must treat unknown
/// codes as non-retryable failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCode {
    // -- connection / negotiation -------------------------------------------
    BlenderNotConnected,
    ProtocolMismatch,
    CapabilityUnavailable,

    // -- input validation ---------------------------------------------------
    InvalidArgument,
    InvalidEnum,
    InvalidTransform,
    InvalidPath,
    InvalidNodeType,
    InvalidNodeSocket,
    InvalidProperty,

    // -- lookups ------------------------------------------------------------
    ObjectNotFound,
    CollectionNotFound,
    MaterialNotFound,
    NodeNotFound,
    NodeTreeNotFound,
    ImageNotFound,
    ArmatureNotFound,
    BoneNotFound,
    ActionNotFound,
    CameraNotFound,
    LightNotFound,
    ModifierNotFound,
    SceneNotFound,
    ArtifactNotFound,

    // -- staleness ----------------------------------------------------------
    TopologyStale,
    RevisionExpired,

    // -- support ------------------------------------------------------------
    UnsupportedOperation,
    UnsupportedFormat,
    UnsupportedProperty,
    UnsupportedBlenderVersion,

    // -- Blender-side failures ----------------------------------------------
    BlenderContextError,
    BlenderModeError,
    BlenderInternalError,

    // -- batching / transactions --------------------------------------------
    TransactionFailed,
    TransactionUnsupported,
    RollbackFailed,

    // -- transport ----------------------------------------------------------
    Timeout,
    ConnectionLost,
    MessageTooLarge,
    RateLimited,

    // -- external assets ----------------------------------------------------
    AssetProviderError,
    AssetNotFound,
    AssetDownloadFailed,
    AssetAuthRequired,
    AssetLicenseRestricted,

    // -- policy -------------------------------------------------------------
    PathNotAllowed,
    PermissionDenied,
}

impl ErrorCode {
    /// Whether retrying the identical request could plausibly succeed without
    /// the caller changing anything. Used as the default for
    /// [`BlenderError::retryable`], and by the client's read-retry policy.
    pub const fn default_retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::BlenderNotConnected
                | ErrorCode::Timeout
                | ErrorCode::ConnectionLost
                | ErrorCode::RateLimited
                | ErrorCode::AssetProviderError
                | ErrorCode::AssetDownloadFailed
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BlenderNotConnected => "BLENDER_NOT_CONNECTED",
            ErrorCode::ProtocolMismatch => "PROTOCOL_MISMATCH",
            ErrorCode::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
            ErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            ErrorCode::InvalidEnum => "INVALID_ENUM",
            ErrorCode::InvalidTransform => "INVALID_TRANSFORM",
            ErrorCode::InvalidPath => "INVALID_PATH",
            ErrorCode::InvalidNodeType => "INVALID_NODE_TYPE",
            ErrorCode::InvalidNodeSocket => "INVALID_NODE_SOCKET",
            ErrorCode::InvalidProperty => "INVALID_PROPERTY",
            ErrorCode::ObjectNotFound => "OBJECT_NOT_FOUND",
            ErrorCode::CollectionNotFound => "COLLECTION_NOT_FOUND",
            ErrorCode::MaterialNotFound => "MATERIAL_NOT_FOUND",
            ErrorCode::NodeNotFound => "NODE_NOT_FOUND",
            ErrorCode::NodeTreeNotFound => "NODE_TREE_NOT_FOUND",
            ErrorCode::ImageNotFound => "IMAGE_NOT_FOUND",
            ErrorCode::ArmatureNotFound => "ARMATURE_NOT_FOUND",
            ErrorCode::BoneNotFound => "BONE_NOT_FOUND",
            ErrorCode::ActionNotFound => "ACTION_NOT_FOUND",
            ErrorCode::CameraNotFound => "CAMERA_NOT_FOUND",
            ErrorCode::LightNotFound => "LIGHT_NOT_FOUND",
            ErrorCode::ModifierNotFound => "MODIFIER_NOT_FOUND",
            ErrorCode::SceneNotFound => "SCENE_NOT_FOUND",
            ErrorCode::ArtifactNotFound => "ARTIFACT_NOT_FOUND",
            ErrorCode::TopologyStale => "TOPOLOGY_STALE",
            ErrorCode::RevisionExpired => "REVISION_EXPIRED",
            ErrorCode::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            ErrorCode::UnsupportedFormat => "UNSUPPORTED_FORMAT",
            ErrorCode::UnsupportedProperty => "UNSUPPORTED_PROPERTY",
            ErrorCode::UnsupportedBlenderVersion => "UNSUPPORTED_BLENDER_VERSION",
            ErrorCode::BlenderContextError => "BLENDER_CONTEXT_ERROR",
            ErrorCode::BlenderModeError => "BLENDER_MODE_ERROR",
            ErrorCode::BlenderInternalError => "BLENDER_INTERNAL_ERROR",
            ErrorCode::TransactionFailed => "TRANSACTION_FAILED",
            ErrorCode::TransactionUnsupported => "TRANSACTION_UNSUPPORTED",
            ErrorCode::RollbackFailed => "ROLLBACK_FAILED",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::ConnectionLost => "CONNECTION_LOST",
            ErrorCode::MessageTooLarge => "MESSAGE_TOO_LARGE",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::AssetProviderError => "ASSET_PROVIDER_ERROR",
            ErrorCode::AssetNotFound => "ASSET_NOT_FOUND",
            ErrorCode::AssetDownloadFailed => "ASSET_DOWNLOAD_FAILED",
            ErrorCode::AssetAuthRequired => "ASSET_AUTH_REQUIRED",
            ErrorCode::AssetLicenseRestricted => "ASSET_LICENSE_RESTRICTED",
            ErrorCode::PathNotAllowed => "PATH_NOT_ALLOWED",
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
        }
    }

    /// The lookup-failure code for a given entity kind name, so generic
    /// resolution code can raise a specific error.
    pub fn not_found_for(kind: &str) -> Self {
        match kind {
            "object" => ErrorCode::ObjectNotFound,
            "collection" => ErrorCode::CollectionNotFound,
            "material" => ErrorCode::MaterialNotFound,
            "node" => ErrorCode::NodeNotFound,
            "node_tree" => ErrorCode::NodeTreeNotFound,
            "image" => ErrorCode::ImageNotFound,
            "armature" => ErrorCode::ArmatureNotFound,
            "bone" => ErrorCode::BoneNotFound,
            "action" => ErrorCode::ActionNotFound,
            "camera" => ErrorCode::CameraNotFound,
            "light" => ErrorCode::LightNotFound,
            "modifier" => ErrorCode::ModifierNotFound,
            "scene" => ErrorCode::SceneNotFound,
            "artifact" => ErrorCode::ArtifactNotFound,
            _ => ErrorCode::InvalidArgument,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured failure.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlenderError {
    pub code: ErrorCode,
    pub message: String,
    /// Machine-readable context. Keys are stable per code; see `docs/PROTOCOL.md`.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub details: Map<String, Value>,
    /// Whether an identical retry could succeed.
    pub retryable: bool,
}

impl BlenderError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Map::new(),
            retryable: code.default_retryable(),
        }
    }

    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Attach a detail from any serialisable value. Serialisation failures are
    /// dropped rather than masking the original error.
    pub fn with_detail_json<T: Serialize>(mut self, key: impl Into<String>, value: &T) -> Self {
        if let Ok(v) = serde_json::to_value(value) {
            self.details.insert(key.into(), v);
        }
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    // -- common constructors ------------------------------------------------

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    /// An enum-valued argument was outside the accepted set. `allowed` is
    /// included verbatim so the caller can pick a legal value immediately.
    pub fn invalid_enum(
        field: &str,
        got: impl Into<String>,
        allowed: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let got = got.into();
        let allowed: Vec<Value> = allowed
            .into_iter()
            .map(|s| Value::String(s.into()))
            .collect();
        Self::new(
            ErrorCode::InvalidEnum,
            format!("`{field}` does not accept the value `{got}`"),
        )
        .with_detail("field", field)
        .with_detail("value", got)
        .with_detail("allowed", Value::Array(allowed))
    }

    pub fn not_found(kind: &str, reference: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::not_found_for(kind),
            format!("No {kind} matches `{reference}`."),
        )
        .with_detail("kind", kind)
        .with_detail("reference", reference.to_string())
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedOperation, message)
    }

    pub fn capability_unavailable(feature: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::CapabilityUnavailable, message).with_detail("feature", feature)
    }

    pub fn not_connected() -> Self {
        Self::new(
            ErrorCode::BlenderNotConnected,
            "No Blender instance is connected. Start Blender with the Blender MCP Bridge add-on enabled, then retry.",
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BlenderInternalError, message)
    }

    pub fn path_not_allowed(path: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::PathNotAllowed,
            format!("`{path}` is outside the configured managed roots."),
        )
        .with_detail("path", path.to_string())
    }
}

impl fmt::Display for BlenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for BlenderError {}

pub type Result<T, E = BlenderError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_screaming_snake() {
        let json = serde_json::to_string(&ErrorCode::InvalidNodeSocket).unwrap();
        assert_eq!(json, "\"INVALID_NODE_SOCKET\"");
        assert_eq!(ErrorCode::InvalidNodeSocket.as_str(), "INVALID_NODE_SOCKET");
    }

    #[test]
    fn invalid_enum_lists_alternatives() {
        let err = BlenderError::invalid_enum("type", "SPHERE", ["CUBE", "UV_SPHERE"]);
        assert_eq!(err.code, ErrorCode::InvalidEnum);
        assert_eq!(
            err.details.get("allowed").unwrap(),
            &serde_json::json!(["CUBE", "UV_SPHERE"])
        );
        assert!(!err.retryable);
    }

    #[test]
    fn transport_failures_default_to_retryable() {
        assert!(BlenderError::new(ErrorCode::Timeout, "slow").retryable);
        assert!(!BlenderError::new(ErrorCode::InvalidArgument, "bad").retryable);
    }
}

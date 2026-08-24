//! Connection handshake and capability negotiation.

use serde::{Deserialize, Serialize};

use crate::{
    capabilities::{BlenderIdentity, Capabilities},
    envelope::SessionId,
    error::{BlenderError, ErrorCode},
    version::{PROTOCOL_VERSION, check_compatibility},
};

/// First frame the server sends after the socket opens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub client_name: String,
    pub client_version: String,
    /// Server-minted session id. The add-on echoes it, and stamps it on every
    /// event, so responses from a previous connection are recognisably stale.
    pub session_id: SessionId,
}

impl Hello {
    pub fn new(
        client_name: impl Into<String>,
        client_version: impl Into<String>,
        session_id: SessionId,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            client_name: client_name.into(),
            client_version: client_version.into(),
            session_id,
        }
    }
}

/// Blender's answer. Carries everything the server needs to validate later
/// requests without asking again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub session_id: SessionId,
    #[serde(flatten)]
    pub identity: BlenderIdentity,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Scene revision at connect time; the cache starts from here.
    #[serde(default)]
    pub revision: u64,
}

impl HelloAck {
    /// Validate the peer's answer against the `Hello` that was sent.
    pub fn validate(&self, sent: &Hello) -> Result<(), BlenderError> {
        if self.session_id != sent.session_id {
            return Err(BlenderError::new(
                ErrorCode::ProtocolMismatch,
                "The add-on answered with a different session id than the one offered; a stale Blender instance may be holding the port.",
            )
            .with_detail("expected", sent.session_id.to_string())
            .with_detail("received", self.session_id.to_string()));
        }

        check_compatibility(self.protocol_version, self.identity.blender_version).map_err(|why| {
            let code = match why {
                crate::version::VersionRejection::BlenderTooOld { .. } => {
                    ErrorCode::UnsupportedBlenderVersion
                }
                _ => ErrorCode::ProtocolMismatch,
            };
            BlenderError::new(code, why.to_string())
                .with_detail("addon_protocol_version", self.protocol_version)
                .with_detail("server_protocol_version", PROTOCOL_VERSION)
                .with_detail("blender_version", self.identity.blender_version.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capabilities::Capabilities, version::BlenderVersion};

    fn ack(session: SessionId, protocol: u32, blender: BlenderVersion) -> HelloAck {
        HelloAck {
            protocol_version: protocol,
            session_id: session,
            identity: BlenderIdentity {
                blender_version: blender,
                python_version: "3.13.0".into(),
                addon_version: "0.1.0".into(),
                platform: "windows".into(),
                background: false,
            },
            capabilities: Capabilities::default(),
            revision: 0,
        }
    }

    #[test]
    fn accepts_a_matching_peer() {
        let hello = Hello::new("rust-blender-mcp", "0.1.0", SessionId::new());
        let ack = ack(
            hello.session_id,
            PROTOCOL_VERSION,
            BlenderVersion::new(4, 2, 0),
        );
        assert!(ack.validate(&hello).is_ok());
    }

    #[test]
    fn rejects_a_stale_session() {
        let hello = Hello::new("rust-blender-mcp", "0.1.0", SessionId::new());
        let ack = ack(
            SessionId::new(),
            PROTOCOL_VERSION,
            BlenderVersion::new(4, 2, 0),
        );
        let err = ack.validate(&hello).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProtocolMismatch);
    }

    #[test]
    fn rejects_unsupported_blender() {
        let hello = Hello::new("rust-blender-mcp", "0.1.0", SessionId::new());
        let ack = ack(
            hello.session_id,
            PROTOCOL_VERSION,
            BlenderVersion::new(3, 6, 0),
        );
        assert_eq!(
            ack.validate(&hello).unwrap_err().code,
            ErrorCode::UnsupportedBlenderVersion
        );
    }

    #[test]
    fn hello_ack_flattens_identity() {
        let json =
            serde_json::to_value(ack(SessionId::new(), 1, BlenderVersion::new(5, 1, 0))).unwrap();
        assert!(
            json.get("blender_version").is_some(),
            "identity must be flattened: {json}"
        );
    }
}

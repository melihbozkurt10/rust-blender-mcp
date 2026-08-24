//! Transport-level errors, and how they map onto the protocol taxonomy.

use std::io;

use blender_protocol::error::{BlenderError, ErrorCode};

/// Failures that belong to the transport rather than to Blender.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not bind {address}: {source}")]
    Bind {
        address: String,
        #[source]
        source: io::Error,
    },
    #[error("the transport task has stopped")]
    Stopped,
    #[error(transparent)]
    Protocol(#[from] BlenderError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<ClientError> for BlenderError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Protocol(inner) => inner,
            ClientError::Bind {
                ref address,
                ref source,
            } => {
                let hint = if source.kind() == io::ErrorKind::AddrInUse {
                    " Another MCP server instance is probably already listening there."
                } else {
                    ""
                };
                BlenderError::new(
                    ErrorCode::BlenderNotConnected,
                    format!("Could not listen on {address}: {source}.{hint}"),
                )
                .with_detail("address", address.clone())
            }
            ClientError::Stopped => BlenderError::new(
                ErrorCode::ConnectionLost,
                "The transport task has stopped; the MCP server needs restarting.",
            ),
            ClientError::Io(source) => {
                BlenderError::new(ErrorCode::ConnectionLost, source.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_in_use_gets_an_actionable_hint() {
        let error = ClientError::Bind {
            address: "127.0.0.1:9877".into(),
            source: io::Error::new(io::ErrorKind::AddrInUse, "in use"),
        };
        let mapped: BlenderError = error.into();
        assert_eq!(mapped.code, ErrorCode::BlenderNotConnected);
        assert!(
            mapped.message.contains("already listening"),
            "{}",
            mapped.message
        );
    }

    #[test]
    fn protocol_errors_pass_through_unchanged() {
        let original = BlenderError::not_found("object", "Cube");
        let mapped: BlenderError = ClientError::Protocol(original.clone()).into();
        assert_eq!(mapped.code, original.code);
        assert_eq!(mapped.details, original.details);
    }
}

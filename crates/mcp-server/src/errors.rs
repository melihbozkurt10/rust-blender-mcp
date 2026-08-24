//! Turning protocol errors into MCP results.
//!
//! MCP draws a line between "the server could not route this" (a JSON-RPC
//! error, which most clients render opaquely) and "the tool ran and failed"
//! (a tool result with `isError`, whose content the model actually sees).
//!
//! Almost everything here is the second kind. A model that asks for a
//! non-existent object needs to *read* the error and the list of objects that
//! do exist, so it can fix its next call; burying that in a protocol error
//! would leave it guessing.

use blender_protocol::error::{BlenderError, ErrorCode};
use rmcp::{
    ErrorData as McpError,
    model::{CallToolResult, ErrorCode as McpErrorCode},
};
use serde_json::{Value, json};

/// Render a failure as a tool-level error the model can act on.
pub fn tool_error(error: &BlenderError) -> CallToolResult {
    CallToolResult::structured_error(error_payload(error))
}

/// The JSON body of an error result.
pub fn error_payload(error: &BlenderError) -> Value {
    let mut payload = json!({
        "error": {
            "code": error.code.as_str(),
            "message": error.message,
            "retryable": error.retryable,
        }
    });
    if !error.details.is_empty() {
        payload["error"]["details"] = Value::Object(error.details.clone());
    }
    if let Some(hint) = hint_for(error) {
        payload["error"]["hint"] = Value::String(hint.to_string());
    }
    payload
}

/// An extra sentence of guidance for the codes where the fix is not obvious
/// from the message alone.
fn hint_for(error: &BlenderError) -> Option<&'static str> {
    match error.code {
        ErrorCode::BlenderNotConnected => Some(
            "Open Blender, enable the Blender MCP Bridge add-on, and check the MCP panel in the 3D viewport sidebar.",
        ),
        ErrorCode::TopologyStale => Some(
            "Re-read the mesh with mesh.info or mesh.analyze to get the current indices and revision, then retry.",
        ),
        ErrorCode::RevisionExpired => Some(
            "The requested revision has fallen out of history. Take a fresh scene.snapshot and diff from there.",
        ),
        ErrorCode::BlenderModeError => Some(
            "Blender was in the wrong mode. This is usually a transient context problem; retrying once often succeeds.",
        ),
        ErrorCode::CapabilityUnavailable => {
            Some("Call blender.capabilities to see what this Blender build supports.")
        }
        ErrorCode::PathNotAllowed | ErrorCode::InvalidPath => {
            Some("Paths are relative to a managed root. Absolute paths and `..` are not accepted.")
        }
        ErrorCode::AssetAuthRequired => Some(
            "Set the provider's API token in the server environment and restart the MCP server.",
        ),
        _ => None,
    }
}

/// A protocol-level error, for failures that are the server's problem rather
/// than the caller's.
pub fn protocol_error(error: &BlenderError) -> McpError {
    McpError::new(
        McpErrorCode::INTERNAL_ERROR,
        error.message.clone(),
        Some(error_payload(error)),
    )
}

/// A malformed request: the caller sent something that is not shaped like the
/// tool's schema at all.
pub fn invalid_params(message: impl Into<String>, details: Option<Value>) -> McpError {
    McpError::invalid_params(message.into(), details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_errors_carry_code_and_details() {
        let error = BlenderError::not_found("object", "Cube");
        let payload = error_payload(&error);
        assert_eq!(payload["error"]["code"], "OBJECT_NOT_FOUND");
        assert_eq!(payload["error"]["details"]["reference"], "Cube");
        assert_eq!(payload["error"]["retryable"], false);
    }

    #[test]
    fn disconnection_explains_what_to_do() {
        let payload = error_payload(&BlenderError::not_connected());
        assert!(
            payload["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("add-on"),
            "expected an actionable hint, got {payload}"
        );
    }

    #[test]
    fn tool_error_marks_the_result_as_an_error() {
        let result = tool_error(&BlenderError::invalid_argument("nope"));
        assert_eq!(result.is_error, Some(true));
        assert!(result.structured_content.is_some());
    }
}

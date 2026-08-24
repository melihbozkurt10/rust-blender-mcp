//! The message envelope exchanged over the persistent socket.
//!
//! Every frame is exactly one JSON object with a `type` discriminator. Requests
//! carry a `request_id`; responses echo it. Ordering is never assumed --
//! multiple requests may be in flight and Blender may answer out of order.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    command::Command,
    error::BlenderError,
    event::Event,
    handshake::{Hello, HelloAck},
};

/// Correlates a request with its response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequestId(Uuid);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Identifies one Blender connection.
///
/// A restarted Blender gets a fresh session id, which is how a late response
/// from a dead instance is distinguished from a live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// A request from the server to Blender.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub request_id: RequestId,
    pub command: Command,
    /// Deadline hint in milliseconds. The bridge abandons work that outlives
    /// it where it safely can, so a slow operation does not wedge the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Blender's answer to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub request_id: RequestId,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BlenderError>,
    /// Scene revision after this operation, when the bridge tracked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl Response {
    pub fn success(request_id: RequestId, result: Value) -> Self {
        Self {
            request_id,
            ok: true,
            result: Some(result),
            error: None,
            revision: None,
        }
    }

    pub fn failure(request_id: RequestId, error: BlenderError) -> Self {
        Self {
            request_id,
            ok: false,
            result: None,
            error: Some(error),
            revision: None,
        }
    }

    /// Collapse the wire form into a `Result`, defending against a peer that
    /// sets `ok: false` without an error payload.
    pub fn into_result(self) -> Result<Value, BlenderError> {
        if self.ok {
            Ok(self.result.unwrap_or(Value::Null))
        } else {
            Err(self.error.unwrap_or_else(|| {
                BlenderError::internal("Blender reported a failure without an error payload.")
            }))
        }
    }
}

/// Every frame that can appear on the socket, in either direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Envelope {
    /// Server -> Blender, first frame after connect.
    Hello(Hello),
    /// Blender -> server, answering [`Envelope::Hello`].
    HelloAck(HelloAck),
    /// Server -> Blender.
    Request(Request),
    /// Blender -> server.
    Response(Response),
    /// Blender -> server, unsolicited scene change notification.
    Event(Event),
    /// Either direction. Used to detect a half-open socket.
    Ping {
        #[serde(default)]
        nonce: u64,
    },
    /// Answer to [`Envelope::Ping`], echoing the nonce.
    Pong {
        #[serde(default)]
        nonce: u64,
    },
    /// Blender -> server, sent when a frame could not be decoded at all and no
    /// `request_id` was recoverable.
    Fatal {
        error: BlenderError,
        #[serde(default)]
        details: Map<String, Value>,
    },
}

impl Envelope {
    /// Discriminator string, for logs and metrics.
    pub const fn type_name(&self) -> &'static str {
        match self {
            Envelope::Hello(_) => "hello",
            Envelope::HelloAck(_) => "hello_ack",
            Envelope::Request(_) => "request",
            Envelope::Response(_) => "response",
            Envelope::Event(_) => "event",
            Envelope::Ping { .. } => "ping",
            Envelope::Pong { .. } => "pong",
            Envelope::Fatal { .. } => "fatal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn request_and_response_round_trip() {
        let id = RequestId::new();
        let env = Envelope::Request(Request {
            request_id: id,
            command: Command::new("object.list"),
            timeout_ms: Some(15_000),
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"request\""));
        let back: Envelope = serde_json::from_str(&json).unwrap();
        match back {
            Envelope::Request(r) => {
                assert_eq!(r.request_id, id);
                assert_eq!(r.command.op, "object.list");
            }
            other => panic!("expected request, got {}", other.type_name()),
        }
    }

    #[test]
    fn failure_without_payload_still_yields_an_error() {
        let resp = Response {
            request_id: RequestId::new(),
            ok: false,
            result: None,
            error: None,
            revision: None,
        };
        let err = resp.into_result().unwrap_err();
        assert_eq!(err.code, ErrorCode::BlenderInternalError);
    }

    #[test]
    fn unknown_envelope_type_is_rejected() {
        let err = serde_json::from_str::<Envelope>(r#"{"type":"exec","code":"import os"}"#);
        assert!(
            err.is_err(),
            "the envelope must not accept unknown frame types"
        );
    }
}

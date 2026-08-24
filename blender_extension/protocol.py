"""Envelope construction and the error taxonomy, mirrored from Rust.

The codes here must stay in step with ``blender_protocol::error::ErrorCode``.
``tests/protocol/test_error_parity.py`` fails if they drift.
"""

from __future__ import annotations

from typing import Any

from . import config


class ErrorCode:
    """Stable error codes. Mirrors the Rust enum."""

    BLENDER_NOT_CONNECTED = "BLENDER_NOT_CONNECTED"
    PROTOCOL_MISMATCH = "PROTOCOL_MISMATCH"
    CAPABILITY_UNAVAILABLE = "CAPABILITY_UNAVAILABLE"

    INVALID_ARGUMENT = "INVALID_ARGUMENT"
    INVALID_ENUM = "INVALID_ENUM"
    INVALID_TRANSFORM = "INVALID_TRANSFORM"
    INVALID_PATH = "INVALID_PATH"
    INVALID_NODE_TYPE = "INVALID_NODE_TYPE"
    INVALID_NODE_SOCKET = "INVALID_NODE_SOCKET"
    INVALID_PROPERTY = "INVALID_PROPERTY"

    OBJECT_NOT_FOUND = "OBJECT_NOT_FOUND"
    COLLECTION_NOT_FOUND = "COLLECTION_NOT_FOUND"
    MATERIAL_NOT_FOUND = "MATERIAL_NOT_FOUND"
    NODE_NOT_FOUND = "NODE_NOT_FOUND"
    NODE_TREE_NOT_FOUND = "NODE_TREE_NOT_FOUND"
    IMAGE_NOT_FOUND = "IMAGE_NOT_FOUND"
    ARMATURE_NOT_FOUND = "ARMATURE_NOT_FOUND"
    BONE_NOT_FOUND = "BONE_NOT_FOUND"
    ACTION_NOT_FOUND = "ACTION_NOT_FOUND"
    CAMERA_NOT_FOUND = "CAMERA_NOT_FOUND"
    LIGHT_NOT_FOUND = "LIGHT_NOT_FOUND"
    MODIFIER_NOT_FOUND = "MODIFIER_NOT_FOUND"
    SCENE_NOT_FOUND = "SCENE_NOT_FOUND"
    ARTIFACT_NOT_FOUND = "ARTIFACT_NOT_FOUND"

    TOPOLOGY_STALE = "TOPOLOGY_STALE"
    REVISION_EXPIRED = "REVISION_EXPIRED"

    UNSUPPORTED_OPERATION = "UNSUPPORTED_OPERATION"
    UNSUPPORTED_FORMAT = "UNSUPPORTED_FORMAT"
    UNSUPPORTED_PROPERTY = "UNSUPPORTED_PROPERTY"
    UNSUPPORTED_BLENDER_VERSION = "UNSUPPORTED_BLENDER_VERSION"

    BLENDER_CONTEXT_ERROR = "BLENDER_CONTEXT_ERROR"
    BLENDER_MODE_ERROR = "BLENDER_MODE_ERROR"
    BLENDER_INTERNAL_ERROR = "BLENDER_INTERNAL_ERROR"

    TRANSACTION_FAILED = "TRANSACTION_FAILED"
    TRANSACTION_UNSUPPORTED = "TRANSACTION_UNSUPPORTED"
    ROLLBACK_FAILED = "ROLLBACK_FAILED"

    TIMEOUT = "TIMEOUT"
    CONNECTION_LOST = "CONNECTION_LOST"
    MESSAGE_TOO_LARGE = "MESSAGE_TOO_LARGE"
    RATE_LIMITED = "RATE_LIMITED"

    ASSET_PROVIDER_ERROR = "ASSET_PROVIDER_ERROR"
    ASSET_NOT_FOUND = "ASSET_NOT_FOUND"
    ASSET_DOWNLOAD_FAILED = "ASSET_DOWNLOAD_FAILED"
    ASSET_AUTH_REQUIRED = "ASSET_AUTH_REQUIRED"
    ASSET_LICENSE_RESTRICTED = "ASSET_LICENSE_RESTRICTED"

    PATH_NOT_ALLOWED = "PATH_NOT_ALLOWED"
    PERMISSION_DENIED = "PERMISSION_DENIED"


#: Codes where an identical retry could plausibly succeed.
RETRYABLE_CODES = frozenset(
    {
        ErrorCode.BLENDER_NOT_CONNECTED,
        ErrorCode.TIMEOUT,
        ErrorCode.CONNECTION_LOST,
        ErrorCode.RATE_LIMITED,
        ErrorCode.ASSET_PROVIDER_ERROR,
        ErrorCode.ASSET_DOWNLOAD_FAILED,
    }
)

#: Which not-found code belongs to which entity kind.
NOT_FOUND_BY_KIND = {
    "object": ErrorCode.OBJECT_NOT_FOUND,
    "collection": ErrorCode.COLLECTION_NOT_FOUND,
    "material": ErrorCode.MATERIAL_NOT_FOUND,
    "node": ErrorCode.NODE_NOT_FOUND,
    "node_tree": ErrorCode.NODE_TREE_NOT_FOUND,
    "image": ErrorCode.IMAGE_NOT_FOUND,
    "armature": ErrorCode.ARMATURE_NOT_FOUND,
    "bone": ErrorCode.BONE_NOT_FOUND,
    "action": ErrorCode.ACTION_NOT_FOUND,
    "camera": ErrorCode.CAMERA_NOT_FOUND,
    "light": ErrorCode.LIGHT_NOT_FOUND,
    "modifier": ErrorCode.MODIFIER_NOT_FOUND,
    "scene": ErrorCode.SCENE_NOT_FOUND,
}


class BridgeError(Exception):
    """A structured failure that crosses the wire intact."""

    def __init__(
        self,
        code: str,
        message: str,
        details: dict[str, Any] | None = None,
        retryable: bool | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = dict(details or {})
        self.retryable = code in RETRYABLE_CODES if retryable is None else retryable

    def to_payload(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
        }
        if self.details:
            payload["details"] = self.details
        return payload

    def __repr__(self) -> str:  # pragma: no cover - diagnostics only
        return f"BridgeError({self.code}, {self.message!r})"


def invalid_argument(message: str, **details: Any) -> BridgeError:
    return BridgeError(ErrorCode.INVALID_ARGUMENT, message, details)


def invalid_enum(field: str, value: Any, allowed) -> BridgeError:
    allowed = sorted(allowed)
    return BridgeError(
        ErrorCode.INVALID_ENUM,
        f"`{field}` does not accept the value `{value}`",
        {"field": field, "value": value, "allowed": allowed},
    )


def not_found(kind: str, reference: Any, **details: Any) -> BridgeError:
    code = NOT_FOUND_BY_KIND.get(kind, ErrorCode.INVALID_ARGUMENT)
    payload = {"kind": kind, "reference": str(reference)}
    payload.update(details)
    return BridgeError(code, f"No {kind} matches `{reference}`.", payload)


def unsupported(message: str, **details: Any) -> BridgeError:
    return BridgeError(ErrorCode.UNSUPPORTED_OPERATION, message, details)


def internal(message: str, **details: Any) -> BridgeError:
    return BridgeError(ErrorCode.BLENDER_INTERNAL_ERROR, message, details)


# --- envelopes -------------------------------------------------------------


def hello_ack(session_id: str, identity: dict[str, Any], capabilities: dict[str, Any], revision: int) -> dict[str, Any]:
    """Answer the server's ``hello``.

    ``identity`` is flattened into the frame to match the Rust ``#[serde(flatten)]``.
    """
    payload = {
        "type": "hello_ack",
        "protocol_version": config.PROTOCOL_VERSION,
        "session_id": session_id,
        "capabilities": capabilities,
        "revision": revision,
    }
    payload.update(identity)
    return payload


def response(request_id: str, result: Any, revision: int | None = None) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "type": "response",
        "request_id": request_id,
        "ok": True,
        "result": result if result is not None else {},
    }
    if revision is not None:
        payload["revision"] = revision
    return payload


def error_response(request_id: str, error: BridgeError, revision: int | None = None) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "type": "response",
        "request_id": request_id,
        "ok": False,
        "error": error.to_payload(),
    }
    if revision is not None:
        payload["revision"] = revision
    return payload


def event(session_id: str, revision: int, name: str, /, **fields: Any) -> dict[str, Any]:
    payload = {
        "type": "event",
        "session_id": session_id,
        "revision": revision,
        "event": name,
    }
    payload.update(fields)
    return payload


def fatal(error: BridgeError, **details: Any) -> dict[str, Any]:
    return {"type": "fatal", "error": error.to_payload(), "details": details}


def pong(nonce: int) -> dict[str, Any]:
    return {"type": "pong", "nonce": nonce}

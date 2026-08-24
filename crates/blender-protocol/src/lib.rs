//! The Blender MCP wire protocol and typed domain payloads.
//!
//! This crate is the contract between the Rust server and the Python bridge
//! add-on. It owns:
//!
//! * stable, UUID-backed [`ids`] for every entity the bridge can address;
//! * the [`envelope`] and [`handshake`] frames exchanged over the socket;
//! * [`capabilities`] negotiation, so the server never asks for a feature the
//!   connected Blender build does not have;
//! * the [`error`] taxonomy, which is stable and machine-readable;
//! * typed payloads for every operation, one module per domain.
//!
//! There is deliberately no operation that carries code. A [`command::Command`]
//! is a name from a closed set plus a validated argument object; the Python
//! side dispatches it through a fixed table of handlers.

#![forbid(unsafe_code)]

pub mod animation;
pub mod asset;
pub mod camera;
pub mod capabilities;
pub mod collection;
pub mod command;
pub mod envelope;
pub mod error;
pub mod event;
pub mod geometry_nodes;
pub mod handshake;
pub mod ids;
pub mod io;
pub mod light;
pub mod material;
pub mod math;
pub mod mesh;
pub mod modifier;
pub mod node_graph;
pub mod object;
pub mod render;
pub mod rig;
pub mod scene;
pub mod uv;
pub mod version;

pub use command::{Category, Command, OpKind};
pub use error::{BlenderError, ErrorCode, Result};
pub use ids::*;
pub use math::{Aabb, Axis, Color4, Finite, Quat, Vec2, Vec3};
pub use version::{BlenderVersion, PROTOCOL_VERSION};

/// Server-side validation performed *before* a request reaches Blender.
///
/// Rejecting bad input here rather than in `bpy` is the whole point of the
/// architecture: the caller gets a precise, machine-readable error instead of a
/// Python traceback, and Blender's main thread is never blocked by work that
/// was always going to fail.
pub trait Validate {
    /// The default accepts everything, so payloads with no cross-field
    /// constraints opt in with a bare `impl Validate for T {}`.
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

/// Implement a no-op [`Validate`] for payloads whose schema already fully
/// constrains them.
#[macro_export]
macro_rules! trivially_valid {
    ($($ty:ty),* $(,)?) => {
        $(impl $crate::Validate for $ty {})*
    };
}

/// Reject a string that would be meaningless as a Blender data-block name.
///
/// Blender truncates names at 63 bytes and silently de-duplicates collisions,
/// so an over-long name comes back different from what was asked for -- which
/// breaks any caller that looks the result up by name.
pub fn check_name(name: &str, field: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(
            BlenderError::invalid_argument(format!("`{field}` must not be empty."))
                .with_detail("field", field),
        );
    }
    if name.len() > 63 {
        return Err(BlenderError::invalid_argument(format!(
            "`{field}` is {} bytes; Blender truncates data-block names at 63.",
            name.len()
        ))
        .with_detail("field", field)
        .with_detail("length", name.len()));
    }
    if name.contains(['\n', '\r', '\0']) {
        return Err(BlenderError::invalid_argument(format!(
            "`{field}` must not contain line breaks or null bytes."
        ))
        .with_detail("field", field));
    }
    Ok(())
}

/// Reject an out-of-range or inverted frame range.
pub fn check_frame_range(start: i32, end: i32) -> Result<()> {
    if end < start {
        return Err(BlenderError::invalid_argument(format!(
            "frame range end ({end}) is before start ({start})."
        ))
        .with_detail("start", start)
        .with_detail("end", end));
    }
    Ok(())
}

/// Common pagination controls. Every listing operation accepts these so a
/// 50 000-object scene cannot be dumped into a model's context by accident.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Page {
    /// Maximum number of entries to return. Server default is 100, hard cap 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque cursor from a previous response's `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Default page size when the caller does not ask for one.
pub const DEFAULT_PAGE_LIMIT: u32 = 100;
/// Hard ceiling on page size, applied after the caller's request.
pub const MAX_PAGE_LIMIT: u32 = 1000;

impl Page {
    /// Effective, clamped limit.
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_PAGE_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT)
    }
}

impl Validate for Page {
    fn validate(&self) -> Result<()> {
        if let Some(0) = self.limit {
            return Err(BlenderError::invalid_argument(
                "`limit` must be at least 1.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_bounded_and_printable() {
        assert!(check_name("Wall", "name").is_ok());
        assert!(check_name("  ", "name").is_err());
        assert!(check_name(&"x".repeat(64), "name").is_err());
        assert!(check_name("Wall\nBroken", "name").is_err());
    }

    #[test]
    fn page_limit_is_clamped_not_rejected() {
        assert_eq!(
            Page {
                limit: Some(99_999),
                cursor: None
            }
            .effective_limit(),
            MAX_PAGE_LIMIT
        );
        assert_eq!(Page::default().effective_limit(), DEFAULT_PAGE_LIMIT);
        assert!(
            Page {
                limit: Some(0),
                cursor: None
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn frame_ranges_must_not_invert() {
        assert!(check_frame_range(1, 250).is_ok());
        assert!(check_frame_range(250, 1).is_err());
    }
}

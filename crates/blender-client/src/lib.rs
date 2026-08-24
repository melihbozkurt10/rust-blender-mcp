//! The persistent transport between the Rust MCP server and the Blender bridge
//! add-on.
//!
//! The server listens on loopback and the add-on dials in. See
//! [`connection`] for why that direction, and `docs/PROTOCOL.md` for the frame
//! format.
//!
//! ```no_run
//! # async fn example() -> Result<(), blender_protocol::BlenderError> {
//! use blender_client::{BlenderClient, Config};
//!
//! let client = BlenderClient::start(Config::default()).await?;
//! client.wait_connected(std::time::Duration::from_secs(30)).await?;
//! let summary = client.call("scene.summary", serde_json::json!({})).await?;
//! # let _ = summary;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod client;
pub mod connection;
pub mod error;
pub mod framing;
pub mod pending;
pub mod reconnect;
pub mod session;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

pub use client::BlenderClient;
pub use error::ClientError;
pub use framing::DEFAULT_MAX_FRAME_BYTES;
pub use session::{Session, Status};

/// Default port the add-on dials into.
pub const DEFAULT_PORT: u16 = 9877;

/// Transport configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to listen on. Loopback only by default -- binding this to a
    /// routable address would expose scene mutation to the network.
    pub bind: SocketAddr,
    /// Largest frame accepted in either direction.
    pub max_frame_bytes: u32,
    /// How long the add-on has to answer the handshake.
    pub handshake_timeout: Duration,
    /// Default deadline for an ordinary request.
    pub request_timeout: Duration,
    /// Depth of the queue into the socket writer.
    pub outbound_queue: usize,
    /// Capacity of the event broadcast channel. Slow subscribers lag rather
    /// than blocking the reader.
    pub event_buffer: usize,
    pub client_name: String,
    pub client_version: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            handshake_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(15),
            outbound_queue: 256,
            event_buffer: 1024,
            client_name: "rust-blender-mcp".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Config {
    /// Whether this configuration would accept connections from off-machine.
    pub fn is_loopback_only(&self) -> bool {
        match self.bind.ip() {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback(),
        }
    }
}

/// How long a given operation should be given, before the caller's own
/// override.
///
/// A render can legitimately take minutes while an object query should never
/// take more than a moment; one global timeout cannot serve both.
pub fn default_timeout_for(op: &str) -> Duration {
    // Longest prefix wins, so `render.viewport_screenshot` gets the screenshot
    // budget rather than the render one.
    const RULES: &[(&str, u64)] = &[
        ("render.viewport_screenshot", 60),
        ("render.execute", 300),
        ("batch.render_cameras", 900),
        ("batch.turntable", 900),
        ("texture.bake", 600),
        ("io.import", 300),
        ("io.export", 300),
        ("batch.export", 600),
        ("asset.download", 600),
        ("asset.import", 600),
        ("workflow.product_turntable", 900),
        ("modifier.apply", 120),
        ("mesh.", 60),
        ("geometry_nodes.scatter", 120),
        ("rig.auto_weights", 300),
        ("rig.parent_mesh", 300),
        ("uv.", 120),
        ("scene.cleanup", 120),
        ("scene.purge_orphans", 120),
        ("workflow.", 300),
        ("batch.execute", 300),
        // A dispatch run holds up to a whole batch worth of operations and
        // executes them inside one main-thread pass, so its deadline has to
        // cover the slowest thing a batch may contain, not the fastest.
        ("batch.dispatch", 600),
    ];

    let seconds = RULES
        .iter()
        .filter(|(prefix, _)| op.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, seconds)| *seconds)
        .unwrap_or(15);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_only() {
        assert!(Config::default().is_loopback_only());
        let exposed = Config {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT),
            ..Config::default()
        };
        assert!(!exposed.is_loopback_only());
    }

    #[test]
    fn timeouts_scale_with_the_operation() {
        assert_eq!(default_timeout_for("object.get"), Duration::from_secs(15));
        assert_eq!(
            default_timeout_for("render.execute"),
            Duration::from_secs(300)
        );
        assert_eq!(default_timeout_for("mesh.extrude"), Duration::from_secs(60));
        assert_eq!(
            default_timeout_for("asset.download"),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn the_most_specific_rule_wins() {
        // `render.viewport_screenshot` must not inherit the five-minute render
        // budget just because it starts with `render.`.
        assert_eq!(
            default_timeout_for("render.viewport_screenshot"),
            Duration::from_secs(60)
        );
    }
}

//! A typed Model Context Protocol server for Blender.
//!
//! The server owns the protocol, the schemas, validation, the domain model and
//! every policy decision. Blender is reached through a persistent localhost
//! connection to a small Python add-on that dispatches to a fixed table of
//! handlers.
//!
//! There is deliberately no tool that executes Python, shell commands or any
//! other caller-supplied code. That is the point of the design, not an
//! omission: the model requests typed operations, and the set of operations is
//! fixed at compile time on the Rust side and at import time on the Python
//! side.

#![forbid(unsafe_code)]

pub mod artifacts;
pub mod config;
pub mod errors;
pub mod registry;
pub mod server;
pub mod state;
pub mod sync;
pub mod tools;

use std::sync::Arc;

pub use config::{Config, ConfigError};
pub use server::BlenderMcpServer;
pub use state::AppState;

use registry::{Activation, Registry};

/// Build the server from a configuration.
pub async fn build(config: Config) -> Result<BlenderMcpServer, ConfigError> {
    config
        .prepare_directories()
        .map_err(|e| ConfigError::Workspace(config.workspace.clone(), e))?;

    let client = blender_client::BlenderClient::start(config.transport())
        .await
        .map_err(|error| {
            // The only way this fails is the listener, and the message from the
            // transport already says which address and why.
            ConfigError::Workspace(
                config.workspace.clone(),
                std::io::Error::other(error.to_string()),
            )
        })?;

    let activation = Activation::from_config(config.eager_tools, &config.default_categories);
    let registry = Arc::new(Registry::new(tools::all(), activation));
    let state = AppState::new(config, client, registry);
    // The cache only stays useful if something keeps it in step with what the
    // user is doing in Blender, so the event pump starts with the server.
    sync::spawn(Arc::clone(&state));
    Ok(BlenderMcpServer::new(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_with_an_ephemeral_port() {
        let mut config = Config::default();
        config.bind.set_port(0);
        config.workspace = std::env::temp_dir().join("blender-mcp-build-test");
        config.project_root = config.workspace.join("project");
        let server = build(config).await.expect("server should build");
        assert!(server.tool_count() > 0, "no tools were registered");
        assert!(
            server.visible_tool_count() > 0,
            "core tools must always be visible"
        );
        assert!(server.visible_tool_count() <= server.tool_count());
    }
}

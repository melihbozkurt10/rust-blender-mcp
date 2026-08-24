//! The `blender-mcp` binary: an MCP server on stdio.

use std::process::ExitCode;

use blender_mcp_server::{Config, build};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> ExitCode {
    // stdout carries the MCP protocol, so every diagnostic goes to stderr.
    // Writing a single stray byte to stdout corrupts the stream and the client
    // disconnects with no useful message.
    let filter = EnvFilter::try_from_env("BLENDER_MCP_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(false)
        .init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("{error}");
            eprintln!("blender-mcp: {error}");
            return ExitCode::from(2);
        }
    };

    let bind = config.bind;
    let workspace = config.workspace.clone();
    let eager = config.eager_tools;
    let categories: Vec<&str> = config.default_categories.iter().map(|c| c.id()).collect();

    let server = match build(config).await {
        Ok(server) => server,
        Err(error) => {
            tracing::error!("{error}");
            eprintln!("blender-mcp: {error}");
            return ExitCode::from(2);
        }
    };

    // Everything a person needs to answer "is it working, and what is it doing"
    // without attaching a debugger. On stderr, because stdout is the protocol
    // and one stray byte there disconnects the client with no useful message.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %bind,
        workspace = %workspace.display(),
        mode = if eager { "eager" } else { "lazy" },
        categories = %categories.join(","),
        tools_registered = server.tool_count(),
        tools_visible = server.visible_tool_count(),
        blender = "waiting for the add-on to connect",
        "blender-mcp ready"
    );

    let service = match server.serve(stdio()).await {
        Ok(service) => service,
        Err(error) => {
            tracing::error!("could not start the MCP transport: {error}");
            return ExitCode::from(1);
        }
    };

    match service.waiting().await {
        Ok(reason) => {
            tracing::info!(?reason, "client disconnected");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!("transport error: {error}");
            ExitCode::from(1)
        }
    }
}

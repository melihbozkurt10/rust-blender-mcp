//! The MCP surface.

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, InitializeResult,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
    },
    service::{RequestContext, RoleServer},
};
use serde_json::Value;

use crate::{errors, state::AppState};

/// The MCP server.
#[derive(Clone)]
pub struct BlenderMcpServer {
    state: Arc<AppState>,
}

impl BlenderMcpServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Shared state, for embedding the server in a larger process.
    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// How many tools this build knows about, enabled or not.
    pub fn tool_count(&self) -> usize {
        self.state.registry.all().len()
    }

    /// How many tools a client would currently see.
    pub fn visible_tool_count(&self) -> usize {
        self.state.registry.visible().len()
    }

    /// Make sure the client will hear about tool-list changes.
    ///
    /// Installed from the first request rather than from `initialize`, because
    /// the SDK owns version negotiation and overriding `initialize` just to
    /// grab the peer would mean reimplementing it.
    fn ensure_notifier(&self, context: &RequestContext<RoleServer>) {
        if self.state.has_notifier() {
            return;
        }
        let peer = context.peer.clone();
        self.state.set_notifier(Arc::new(move || {
            let peer = peer.clone();
            tokio::spawn(async move {
                if let Err(error) = peer.notify_tool_list_changed().await {
                    tracing::debug!(
                        %error,
                        "client did not accept a tool-list-changed notification"
                    );
                }
            });
        }));
    }

    /// Instructions shown to the model alongside the tool list.
    fn instructions(&self) -> String {
        let registry = &self.state.registry;
        let eager = registry.activation().is_eager();
        let enabled = registry.activation().enabled().ids().join(", ");
        let mut text = String::new();
        text.push_str(
            "Typed Blender automation. Every operation is a validated tool call -- there is no \
             tool that runs Python, shell commands or arbitrary code, and none will be added, so \
             do not look for one.\n\n",
        );
        text.push_str(
            "Start with `scene.summary` to see what is in the file, and `blender.status` if \
             anything reports that Blender is not connected.\n\n",
        );
        if eager {
            text.push_str("All tool categories are registered (eager mode).\n");
        } else {
            text.push_str(&format!(
                "Tools load by category to keep the list small. Currently enabled: {enabled}. \
                 Call `tools.categories.list` to see the rest and `tools.categories.enable` to \
                 add one -- for example enable `materials` before building a shader.\n",
            ));
        }
        text.push_str(
            "\nObjects, materials and other entities are addressed by their stable `id`, which \
             survives renames. A name works too, but only until someone renames it.\n",
        );
        text.push_str(
            "\nErrors are structured: the `code` says what went wrong and `details` usually \
             carries the values that would have been valid. Read them rather than retrying \
             blindly.",
        );
        text
    }
}

impl ServerHandler for BlenderMcpServer {
    fn get_info(&self) -> InitializeResult {
        let server_info = Implementation::new("blender-mcp", env!("CARGO_PKG_VERSION"))
            .with_title("Blender MCP")
            .with_description(
                "Typed Blender automation: modelling, materials, node graphs, animation, \
                 rigging, rendering and import/export.",
            );

        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(server_info)
        .with_instructions(self.instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.ensure_notifier(&context);
        // The list is small enough that paginating it would cost a round trip
        // for nothing: even in eager mode it is a few hundred entries.
        let tools: Vec<Tool> = self
            .state
            .registry
            .visible()
            .iter()
            .map(|t| t.to_tool())
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.state.registry.get(name).map(|tool| tool.to_tool())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.ensure_notifier(&context);
        let name = request.name.to_string();
        let Some(spec) = self.state.registry.get(&name) else {
            // An unknown tool is a routing failure, which is the one case that
            // genuinely belongs at the protocol level.
            return Err(errors::invalid_params(
                format!("`{name}` is not a tool this server provides."),
                Some(serde_json::json!({
                    "tool": name,
                    "hint": "Call tools.categories.list to see every category and enable the one you need.",
                })),
            ));
        };

        let arguments = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let handler = Arc::clone(&spec.handler);
        let kind = spec.kind;
        let state = Arc::clone(&self.state);

        tracing::debug!(tool = %name, kind = kind.as_str(), "calling tool");
        let result = handler(state, arguments).await;

        Ok(match result {
            Ok(value) => CallToolResult::structured(value).into(),
            Err(error) => {
                tracing::info!(tool = %name, code = %error.code, "tool failed: {}", error.message);
                errors::tool_error(&error).into()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use blender_client::BlenderClient;
    use blender_protocol::command::Category;
    use tokio::net::TcpListener;

    use super::*;
    use crate::{
        config::Config,
        registry::{Activation, Registry},
        tools,
    };

    async fn server(eager: bool) -> BlenderMcpServer {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client = BlenderClient::from_listener(blender_client::Config::default(), listener);
        let activation = Activation::from_config(eager, &[Category::Core]);
        let registry = Arc::new(Registry::new(tools::all(), activation));
        BlenderMcpServer::new(AppState::new(Config::default(), client, registry))
    }

    #[tokio::test]
    async fn lazy_mode_exposes_only_core() {
        let server = server(false).await;
        let visible = server.state.registry.visible();
        assert!(!visible.is_empty());
        assert!(
            visible.iter().all(|t| t.category == Category::Core),
            "lazy mode must start with core only"
        );
    }

    #[tokio::test]
    async fn eager_mode_exposes_everything() {
        let server = server(true).await;
        assert_eq!(
            server.state.registry.visible().len(),
            server.state.registry.all().len()
        );
    }

    #[tokio::test]
    async fn enabling_a_category_widens_the_visible_list() {
        let server = server(false).await;
        let before = server.state.registry.visible().len();
        server.state.registry.activation().enable(Category::Scene);
        let after = server.state.registry.visible().len();
        assert!(after > before, "enabling `scene` should reveal more tools");
    }

    #[tokio::test]
    async fn instructions_mention_the_absence_of_code_execution() {
        let server = server(false).await;
        let instructions = server.get_info().instructions.unwrap();
        assert!(
            instructions.contains(
                "no \
             tool that runs Python"
            ) || instructions.contains("runs Python")
        );
    }

    #[tokio::test]
    async fn info_declares_tool_list_change_support() {
        let server = server(false).await;
        let capabilities = server.get_info().capabilities;
        let tools = capabilities.tools.expect("tools capability");
        assert_eq!(tools.list_changed, Some(true));
    }
}

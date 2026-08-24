//! Core tools: connection state, capabilities and tool-category control.
//!
//! These are the only tools registered before a client asks for more, so they
//! have to be enough to find out what is going on and to turn the rest on.

use std::sync::Arc;

use blender_protocol::{
    BlenderError, ErrorCode, Validate,
    command::{Category, OpKind},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::NoParams;
use crate::{registry::ToolSpec, state::AppState};

/// `blender.capabilities`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CapabilitiesParams {
    /// Return the full identifier lists rather than counts. These run to
    /// several hundred entries, so the default is counts plus the identifiers
    /// most callers actually branch on.
    #[serde(default)]
    pub verbose: bool,
}

impl Validate for CapabilitiesParams {}

/// `tools.categories.enable` / `tools.categories.disable`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CategoryParams {
    /// Category id, from `tools.categories.list`.
    pub category: String,
}

impl Validate for CategoryParams {
    fn validate(&self) -> blender_protocol::Result<()> {
        if Category::parse(&self.category).is_none() {
            return Err(BlenderError::invalid_enum(
                "category",
                self.category.clone(),
                Category::ALL.map(|c| c.id()),
            ));
        }
        Ok(())
    }
}

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::custom::<NoParams, _, _>(
            "blender.status",
            Category::Core,
            OpKind::Read,
            "Blender connection status",
            "Report whether Blender is connected, which version and add-on are running, and how \
             many requests are in flight. Always safe to call, including before Blender starts.",
            |state: Arc<AppState>, _params| async move {
                let status = state.client.status();
                let mut value = serde_json::to_value(&status)
                    .map_err(|e| BlenderError::internal(e.to_string()))?;
                if !status.connected {
                    value["how_to_connect"] = json!({
                        "steps": [
                            "Start Blender.",
                            "Enable the `Blender MCP Bridge` add-on in Preferences > Add-ons.",
                            format!("Confirm the add-on is pointed at {}.", status.listen_address),
                            "Open the MCP panel in the 3D viewport sidebar (press N) to see its status.",
                        ],
                    });
                }
                Ok(value)
            },
        ),
        ToolSpec::custom::<CapabilitiesParams, _, _>(
            "blender.capabilities",
            Category::Core,
            OpKind::Read,
            "Blender capabilities",
            "List what the connected Blender build supports: render engines, modifier types, node \
             types, import and export formats, and bake passes. Check this before using anything \
             version-dependent.",
            |state: Arc<AppState>, params: CapabilitiesParams| async move {
                let capabilities = state.capabilities()?;
                let session = state.client.session()?;
                let mut payload = json!({
                    "blender_version": session.identity.blender_version.to_string(),
                    "python_version": session.identity.python_version,
                    "addon_version": session.identity.addon_version,
                    "platform": session.identity.platform,
                    "background": session.identity.background,
                    "render_engines": capabilities.render_engines,
                    "import_formats": capabilities.import_formats,
                    "export_formats": capabilities.export_formats,
                    "bake_types": capabilities.bake_types,
                    "image_formats": capabilities.image_formats,
                    "features": capabilities.features,
                });
                if params.verbose {
                    payload["modifiers"] = json!(capabilities.modifiers);
                    payload["shader_nodes"] = json!(capabilities.shader_nodes);
                    payload["geometry_nodes"] = json!(capabilities.geometry_nodes);
                    payload["constraints"] = json!(capabilities.constraints);
                } else {
                    payload["counts"] = json!({
                        "modifiers": capabilities.modifiers.len(),
                        "shader_nodes": capabilities.shader_nodes.len(),
                        "geometry_nodes": capabilities.geometry_nodes.len(),
                        "constraints": capabilities.constraints.len(),
                    });
                    payload["modifiers"] = json!(capabilities.modifiers);
                    payload["note"] =
                        json!("Pass verbose:true for the full node and constraint type lists.");
                }
                Ok(payload)
            },
        ),
        ToolSpec::custom::<NoParams, _, _>(
            "tools.categories.list",
            Category::Core,
            OpKind::Read,
            "List tool categories",
            "List every tool category, whether it is currently enabled, and how many tools it \
             holds. Enable a category to make its tools callable by name in the tool list.",
            |state: Arc<AppState>, _params| async move {
                let registry = Arc::clone(&state.registry);
                Ok(json!({
                    "mode": if registry.activation().is_eager() { "eager" } else { "lazy" },
                    "categories": registry.categories(),
                    "enabled": registry.activation().enabled().ids(),
                    "visible_tools": registry.visible().len(),
                    "total_tools": registry.all().len(),
                }))
            },
        ),
        ToolSpec::custom::<CategoryParams, _, _>(
            "tools.categories.enable",
            Category::Core,
            OpKind::Read,
            "Enable a tool category",
            "Add a category's tools to the tool list. The client is notified that the tool list \
             changed; if yours does not refresh, start the server with BLENDER_MCP_EAGER_TOOLS=1.",
            |state: Arc<AppState>, params: CategoryParams| async move {
                let category = Category::parse(&params.category).expect("validated");
                let registry = Arc::clone(&state.registry);
                let changed = registry.activation().enable(category);
                if changed {
                    state.notify_tool_list_changed();
                }
                Ok(json!({
                    "category": category.id(),
                    "enabled": true,
                    "changed": changed,
                    "tools": registry.tools_in(category),
                    "eager_mode": registry.activation().is_eager(),
                }))
            },
        ),
        ToolSpec::custom::<CategoryParams, _, _>(
            "tools.categories.disable",
            Category::Core,
            OpKind::Read,
            "Disable a tool category",
            "Remove a category's tools from the tool list to reclaim context. Already-known tools \
             keep working if called by name; only the listing changes.",
            |state: Arc<AppState>, params: CategoryParams| async move {
                let category = Category::parse(&params.category).expect("validated");
                let registry = Arc::clone(&state.registry);
                match registry.activation().disable(category) {
                    Ok(changed) => {
                        if changed {
                            state.notify_tool_list_changed();
                        }
                        Ok(json!({
                            "category": category.id(),
                            "enabled": false,
                            "changed": changed,
                        }))
                    }
                    Err(reason) => Err(BlenderError::new(
                        ErrorCode::InvalidArgument,
                        reason.to_string(),
                    )
                    .with_detail("category", category.id())),
                }
            },
        ),
    ]
}

/// Summarise a value's size, for logging without dumping payloads.
pub fn describe_size(value: &Value) -> usize {
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_params_reject_unknown_ids() {
        let params = CategoryParams {
            category: "not_a_category".into(),
        };
        let error = params.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidEnum);
        let allowed = error.details["allowed"].as_array().unwrap();
        assert!(allowed.iter().any(|v| v == "mesh"));
    }

    #[test]
    fn category_params_accept_known_ids() {
        for category in Category::ALL {
            let params = CategoryParams {
                category: category.id().to_string(),
            };
            assert!(params.validate().is_ok(), "{}", category.id());
        }
    }

    #[test]
    fn core_tools_are_all_in_the_core_category() {
        for tool in tools() {
            assert_eq!(tool.category, Category::Core, "{}", tool.name);
        }
    }
}

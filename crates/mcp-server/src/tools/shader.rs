//! Shader node graph tools.
//!
//! Generic graph editing rather than a fixed menu of presets. Node types are
//! validated against what the connected Blender build actually registers, so
//! asking for a node that does not exist fails with the near-misses listed
//! rather than with a Python traceback.

use std::sync::Arc;

use blender_protocol::{
    BlenderError,
    capabilities::CapabilityKind,
    command::{Category, OpKind},
    node_graph::{
        ClearTree, CreateLink, CreateNode, DeleteLink, DeleteNode, GetNode, GetSocket, GetTree,
        ListLinks, ListNodes, SetSocketDefault, UpdateNode,
    },
};
use serde_json::Value;

use crate::{registry::ToolSpec, state::AppState};

const SHADERS: Category = Category::ShaderNodes;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<GetTree>(
            "shader.tree.get",
            SHADERS,
            OpKind::Read,
            "Get a shader graph",
            "The nodes and links of a material, world or node group. Socket defaults, node \
             properties and editor layout are omitted unless asked for, so the default response \
             stays small.",
        ),
        ToolSpec::forward::<ClearTree>(
            "shader.tree.clear",
            SHADERS,
            OpKind::Write,
            "Clear a shader graph",
            "Delete every node in the tree, keeping the output node by default so the graph can \
             be rebuilt onto it.",
        ),
        ToolSpec::forward::<ListNodes>(
            "shader.node.list",
            SHADERS,
            OpKind::Read,
            "List shader nodes",
            "The nodes in a shader tree, without their sockets. Paginated.",
        ),
        ToolSpec::forward::<GetNode>(
            "shader.node.get",
            SHADERS,
            OpKind::Read,
            "Get a shader node",
            "One node in full: every input and output socket with its identifier, index, type, \
             link state and current default, plus the node type-specific properties.",
        ),
        // Node creation is the one shader tool with server-side logic: the node
        // type is checked against the connected build before the request is
        // sent, so a typo comes back with suggestions instead of a traceback.
        ToolSpec::custom::<CreateNode, _, _>(
            "shader.node.create",
            SHADERS,
            OpKind::Write,
            "Create a shader node",
            "Add a node of any registered shader type, optionally setting its properties and \
             input defaults in the same call. Use `blender.capabilities` with verbose:true for \
             the full list of node types.",
            |state: Arc<AppState>, params: CreateNode| async move {
                state.require_capability(CapabilityKind::ShaderNode, &params.node_type)?;
                state.call_typed("shader.node.create", &params).await
            },
        ),
        ToolSpec::forward::<UpdateNode>(
            "shader.node.update",
            SHADERS,
            OpKind::Write,
            "Update a shader node",
            "Rename, move, mute, or set the properties and input defaults of an existing node. \
             Property names are checked against the node type.",
        ),
        ToolSpec::forward::<DeleteNode>(
            "shader.node.delete",
            SHADERS,
            OpKind::Write,
            "Delete a shader node",
            "Remove a node. Deleting the tree output is refused unless forced, because it leaves \
             the material rendering as flat black.",
        ),
        ToolSpec::forward::<ListLinks>(
            "shader.link.list",
            SHADERS,
            OpKind::Read,
            "List shader links",
            "Every link in the tree, as node id and socket identifier pairs.",
        ),
        ToolSpec::forward::<CreateLink>(
            "shader.link.create",
            SHADERS,
            OpKind::Write,
            "Link two shader sockets",
            "Connect an output socket to an input socket. Blender allows one link per input, so \
             an existing one is replaced by default rather than silently dropped.",
        ),
        ToolSpec::forward::<DeleteLink>(
            "shader.link.delete",
            SHADERS,
            OpKind::Write,
            "Delete shader links",
            "Remove the link feeding an input socket, every link leaving an output socket, or the \
             one link between a specific pair.",
        ),
        ToolSpec::forward::<GetSocket>(
            "shader.socket.get",
            SHADERS,
            OpKind::Read,
            "Get a shader socket",
            "One socket, with its identifier, index, type, link state and current default value.",
        ),
        ToolSpec::forward::<SetSocketDefault>(
            "shader.socket.set_default",
            SHADERS,
            OpKind::Write,
            "Set a socket default",
            "Set the value an unconnected input socket uses. Refused on a linked socket unless \
             forced, because the value would have no effect.",
        ),
    ]
}

/// Whether a value looks like a shader node type identifier.
///
/// Used by the workflow layer when it builds a graph plan from a template, so
/// a malformed template fails before any node is created.
pub fn is_node_type(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// Capability check shared by shader and geometry node creation.
pub async fn require_node_type(
    state: &AppState,
    kind: CapabilityKind,
    node_type: &str,
) -> Result<(), BlenderError> {
    state.require_capability(kind, node_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shader_tool_is_in_its_own_category() {
        for tool in tools() {
            assert_eq!(tool.category, SHADERS, "{}", tool.name);
            assert!(tool.name.starts_with("shader."), "{}", tool.name);
        }
    }

    #[test]
    fn reads_and_writes_are_classified_correctly() {
        for tool in tools() {
            let expected = if tool.name.contains(".get") || tool.name.contains(".list") {
                OpKind::Read
            } else {
                OpKind::Write
            };
            assert_eq!(tool.kind, expected, "{}", tool.name);
        }
    }

    #[test]
    fn node_type_shape_is_checked() {
        assert!(is_node_type(&Value::String("ShaderNodeTexNoise".into())));
        assert!(!is_node_type(&Value::String("Shader Node".into())));
        assert!(!is_node_type(&Value::String(String::new())));
        assert!(!is_node_type(&Value::Null));
    }
}

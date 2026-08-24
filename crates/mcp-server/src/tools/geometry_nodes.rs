//! Geometry node tools.

use std::sync::Arc;

use blender_protocol::{
    capabilities::CapabilityKind,
    command::{Category, OpKind},
    geometry_nodes::{
        AddInterfaceSocket, AttachNodeGroup, BuildGraph, CreateNodeGroup, DeleteInterfaceSocket,
        DeleteNodeGroup, GeometryModifierRef, GroupRefParams, ListNodeGroups,
        UpdateInterfaceSocket,
    },
    node_graph::{
        CreateLink, CreateNode, DeleteLink, DeleteNode, GetNode, GetTree, ListLinks, ListNodes,
        UpdateNode,
    },
};

use crate::{registry::ToolSpec, state::AppState};

const GEO: Category = Category::GeometryNodes;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListNodeGroups>(
            "geometry_nodes.group.list",
            GEO,
            OpKind::Read,
            "List node groups",
            "Geometry node groups in the file, optionally only those attached to one object.",
        ),
        ToolSpec::forward::<GroupRefParams>(
            "geometry_nodes.group.get",
            GEO,
            OpKind::Read,
            "Get a node group",
            "One group with its exposed inputs and outputs and the objects using it.",
        ),
        ToolSpec::forward::<CreateNodeGroup>(
            "geometry_nodes.group.create",
            GEO,
            OpKind::Write,
            "Create a node group",
            "Create a geometry node group, wired input-to-output by default, and optionally attach \
             it to an object in the same call.",
        ),
        ToolSpec::forward::<DeleteNodeGroup>(
            "geometry_nodes.group.delete",
            GEO,
            OpKind::Write,
            "Delete a node group",
            "Delete a group. Refused while modifiers still use it, unless forced.",
        ),
        ToolSpec::forward::<GetTree>(
            "geometry_nodes.tree.get",
            GEO,
            OpKind::Read,
            "Get a geometry graph",
            "The nodes and links of a geometry node group, or of the group driving an object's \
             modifier.",
        ),
        ToolSpec::forward::<ListNodes>(
            "geometry_nodes.node.list",
            GEO,
            OpKind::Read,
            "List geometry nodes",
            "The nodes in a geometry tree, without their sockets. Paginated.",
        ),
        ToolSpec::forward::<GetNode>(
            "geometry_nodes.node.get",
            GEO,
            OpKind::Read,
            "Get a geometry node",
            "One node with every socket and property.",
        ),
        ToolSpec::custom::<CreateNode, _, _>(
            "geometry_nodes.node.create",
            GEO,
            OpKind::Write,
            "Create a geometry node",
            "Add a node of any registered geometry or function node type, configuring its \
             properties and input defaults in the same call.",
            |state: Arc<AppState>, params: CreateNode| async move {
                state.require_capability(CapabilityKind::GeometryNode, &params.node_type)?;
                state
                    .call_typed("geometry_nodes.node.create", &params)
                    .await
            },
        ),
        ToolSpec::forward::<UpdateNode>(
            "geometry_nodes.node.update",
            GEO,
            OpKind::Write,
            "Update a geometry node",
            "Rename, move, mute or reconfigure an existing node.",
        ),
        ToolSpec::forward::<DeleteNode>(
            "geometry_nodes.node.delete",
            GEO,
            OpKind::Write,
            "Delete a geometry node",
            "Remove a node from a geometry tree.",
        ),
        ToolSpec::forward::<ListLinks>(
            "geometry_nodes.link.list",
            GEO,
            OpKind::Read,
            "List geometry links",
            "Every link in a geometry tree.",
        ),
        ToolSpec::forward::<CreateLink>(
            "geometry_nodes.link.create",
            GEO,
            OpKind::Write,
            "Link two geometry sockets",
            "Connect an output to an input, replacing whatever fed that input.",
        ),
        ToolSpec::forward::<DeleteLink>(
            "geometry_nodes.link.delete",
            GEO,
            OpKind::Write,
            "Delete geometry links",
            "Remove links by their source, their destination, or both.",
        ),
        ToolSpec::custom::<BuildGraph, _, _>(
            "geometry_nodes.graph.build",
            GEO,
            OpKind::Write,
            "Build a whole graph",
            "Create many nodes and the links between them in one pass, referring to nodes by \
             caller-chosen keys. Faster and far less error-prone than a sequence of single-node \
             calls, and it is what the scatter and array workflows use.",
            |state: Arc<AppState>, params: BuildGraph| async move {
                // Every node type is checked before any node is created, so a
                // typo in the twentieth node does not leave nineteen behind.
                for node in &params.nodes {
                    state.require_capability(CapabilityKind::GeometryNode, &node.node_type)?;
                }
                state
                    .call_typed("geometry_nodes.graph.build", &params)
                    .await
            },
        ),
        ToolSpec::forward::<GroupRefParams>(
            "geometry_nodes.interface.list",
            GEO,
            OpKind::Read,
            "List group inputs and outputs",
            "The sockets a group exposes on its modifier panel, with their types and bounds.",
        ),
        ToolSpec::forward::<AddInterfaceSocket>(
            "geometry_nodes.interface.add_socket",
            GEO,
            OpKind::Write,
            "Expose a group socket",
            "Add an input or output to a group's interface, with a type, default and optional \
             bounds.",
        ),
        ToolSpec::forward::<UpdateInterfaceSocket>(
            "geometry_nodes.interface.update_socket",
            GEO,
            OpKind::Write,
            "Update a group socket",
            "Rename an exposed socket or change its default and bounds.",
        ),
        ToolSpec::forward::<DeleteInterfaceSocket>(
            "geometry_nodes.interface.delete_socket",
            GEO,
            OpKind::Write,
            "Remove a group socket",
            "Take an input or output off a group's interface.",
        ),
        ToolSpec::forward::<AttachNodeGroup>(
            "geometry_nodes.modifier.attach",
            GEO,
            OpKind::Write,
            "Attach a node group",
            "Add a geometry nodes modifier running a group, and set its exposed inputs by name.",
        ),
        ToolSpec::forward::<GeometryModifierRef>(
            "geometry_nodes.modifier.detach",
            GEO,
            OpKind::Write,
            "Detach a node group",
            "Remove a geometry nodes modifier from an object.",
        ),
        ToolSpec::forward::<GeometryModifierRef>(
            "geometry_nodes.modifier.list",
            GEO,
            OpKind::Read,
            "List geometry modifiers",
            "The geometry nodes modifiers on an object and which group each runs.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_builder_is_present_and_documented() {
        let build = tools()
            .into_iter()
            .find(|t| t.name == "geometry_nodes.graph.build")
            .expect("graph builder");
        assert_eq!(build.kind, OpKind::Write);
        assert!(build.description.contains("one pass"));
    }

    #[test]
    fn geometry_tools_share_the_node_graph_types() {
        // The shader and geometry node tools must present the same socket
        // addressing, or a caller has to learn it twice.
        let geo = tools()
            .into_iter()
            .find(|t| t.name == "geometry_nodes.link.create")
            .unwrap();
        let shader = crate::tools::shader::tools()
            .into_iter()
            .find(|t| t.name == "shader.link.create")
            .unwrap();
        assert_eq!(
            serde_json::to_string(&*geo.schema).unwrap(),
            serde_json::to_string(&*shader.schema).unwrap()
        );
    }
}

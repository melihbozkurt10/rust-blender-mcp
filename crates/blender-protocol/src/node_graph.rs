//! A generic node graph model, shared by shader trees and geometry node trees.
//!
//! Blender's node system is uniform enough that one set of types covers both:
//! nodes have a `bl_idname`, a location, named properties, and typed input and
//! output sockets connected by links. The differences (which node types exist,
//! which tree a node lives in) are handled by capability lookups and by the
//! `domain` field, not by duplicating the model.
//!
//! Socket addressing is the part that bites. Display names are localised and
//! were renamed wholesale in Blender 4.0, and several nodes expose more than
//! one socket with the same display name (`Mix` has two `A` sockets of
//! different types). So a socket is addressed by identifier or index where the
//! caller knows one, and by name only as a convenience -- with an ambiguous
//! name producing a structured error listing the candidates rather than a
//! silent wrong pick.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, ErrorCode, Result, Validate,
    ids::{ImageRef, MaterialRef, NodeId, NodeRef, NodeTreeRef, ObjectRef},
    math::{Color4, Finite, Vec2, Vec3},
};

/// Which family of node tree an operation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphDomain {
    Shader,
    Geometry,
    Compositor,
    World,
}

impl GraphDomain {
    /// The capability list node types for this domain are checked against.
    pub const fn capability(self) -> crate::capabilities::CapabilityKind {
        match self {
            GraphDomain::Shader | GraphDomain::World => {
                crate::capabilities::CapabilityKind::ShaderNode
            }
            GraphDomain::Geometry => crate::capabilities::CapabilityKind::GeometryNode,
            // The compositor shares the shader node registry closely enough
            // that its own list is not maintained separately.
            GraphDomain::Compositor => crate::capabilities::CapabilityKind::ShaderNode,
        }
    }
}

/// Which tree an operation applies to. Exactly one variant is meaningful per
/// domain, which keeps callers from having to know Blender's data-block layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TreeTarget {
    /// The node tree of a material.
    Material(MaterialRef),
    /// A standalone node group, addressed directly.
    NodeTree(NodeTreeRef),
    /// The geometry node group driving a modifier on an object.
    ObjectModifier {
        object: ObjectRef,
        /// Modifier name. Omit when the object has exactly one nodes modifier.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifier: Option<String>,
    },
    /// The active scene's world shader tree.
    World,
}

/// Socket direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SocketDirection {
    Input,
    Output,
}

/// How a socket on a node is identified.
///
/// Prefer `identifier` (stable across Blender versions and localisation) or
/// `index`. `name` is accepted for ergonomics and resolved server-side, with
/// ambiguity reported rather than guessed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SocketSelector {
    /// Blender's stable socket identifier, e.g. `Base Color` or `Vector_001`.
    Identifier(String),
    /// Zero-based position within the node's inputs or outputs.
    Index(u32),
    /// Display name. Convenient, but ambiguous on some nodes.
    Name(String),
}

/// A fully-qualified socket address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SocketAddress {
    pub node: NodeRef,
    #[serde(flatten)]
    pub socket: SocketSelector,
    pub direction: SocketDirection,
}

impl SocketAddress {
    pub fn input(node: NodeRef, socket: SocketSelector) -> Self {
        Self {
            node,
            socket,
            direction: SocketDirection::Input,
        }
    }

    pub fn output(node: NodeRef, socket: SocketSelector) -> Self {
        Self {
            node,
            socket,
            direction: SocketDirection::Output,
        }
    }
}

/// A typed node property or socket default value.
///
/// This enum is the reason there is no `setattr(node, name, value)` anywhere:
/// the property name is checked against the node's advertised properties, and
/// the value carries its own type, so a string can never land in a float
/// socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PropertyValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Vec2(Vec2),
    Vec3(Vec3),
    Color(Color4),
    /// An enum value, validated against the property's items by the bridge.
    Enum(String),
    /// Reference to an image data-block, for texture nodes.
    Image(ImageRef),
    /// Reference to an object, for object-info and texture-coordinate nodes.
    Object(ObjectRef),
    /// Reference to a material.
    Material(MaterialRef),
    /// Reference to a collection, which geometry nodes instance from.
    Collection(crate::ids::CollectionRef),
    /// Reference to a node group to instance.
    NodeGroup(NodeTreeRef),
}

impl PropertyValue {
    /// Human-readable type name, used in mismatch errors.
    pub const fn type_name(&self) -> &'static str {
        match self {
            PropertyValue::Bool(_) => "bool",
            PropertyValue::Int(_) => "int",
            PropertyValue::Float(_) => "float",
            PropertyValue::String(_) => "string",
            PropertyValue::Vec2(_) => "vec2",
            PropertyValue::Vec3(_) => "vec3",
            PropertyValue::Color(_) => "color",
            PropertyValue::Enum(_) => "enum",
            PropertyValue::Image(_) => "image",
            PropertyValue::Object(_) => "object",
            PropertyValue::Material(_) => "material",
            PropertyValue::Collection(_) => "collection",
            PropertyValue::NodeGroup(_) => "node_group",
        }
    }
}

impl Finite for PropertyValue {
    fn check_finite(&self, field: &str) -> Result<()> {
        match self {
            PropertyValue::Float(v) => crate::math::check_scalar_finite(*v, field),
            PropertyValue::Vec2(v) => v.check_finite(field),
            PropertyValue::Vec3(v) => v.check_finite(field),
            PropertyValue::Color(v) => v.check_finite(field),
            _ => Ok(()),
        }
    }
}

/// One named property assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertyAssignment {
    /// Property name as it appears on the node, e.g. `operation`, `blend_type`.
    pub name: String,
    pub value: PropertyValue,
}

impl Validate for PropertyAssignment {
    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(BlenderError::new(
                ErrorCode::InvalidProperty,
                "Property name must not be empty.",
            ));
        }
        // Blender property names are Python identifiers. Anything else is
        // either a typo or an attempt to reach somewhere it should not.
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || self.name.starts_with(|c: char| c.is_ascii_digit())
        {
            return Err(BlenderError::new(
                ErrorCode::InvalidProperty,
                format!("`{}` is not a valid node property name.", self.name),
            )
            .with_detail("property", self.name.clone()));
        }
        if self.name.starts_with("__") {
            return Err(BlenderError::new(
                ErrorCode::PermissionDenied,
                "Dunder attributes are not addressable.",
            )
            .with_detail("property", self.name.clone()));
        }
        self.value
            .check_finite(&format!("value of `{}`", self.name))
    }
}

/// `*.node.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateNode {
    #[serde(flatten)]
    pub tree: TreeTarget,
    /// Blender node type identifier, e.g. `ShaderNodeBsdfPrincipled`.
    pub node_type: String,
    /// Label for the node. Also used as its name where Blender allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Position in the node editor. Purely cosmetic, but a graph laid out in a
    /// column is far easier for a human to inspect afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec2>,
    /// Properties to set immediately after creation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
    /// Input socket defaults to set immediately after creation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<SocketDefault>,
}

impl Validate for CreateNode {
    fn validate(&self) -> Result<()> {
        if self.node_type.is_empty() {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeType,
                "`node_type` must not be empty.",
            ));
        }
        // Node identifiers are CamelCase Python class names. Rejecting
        // everything else keeps arbitrary strings out of `nodes.new()`.
        if !self
            .node_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeType,
                format!("`{}` is not a valid node type identifier.", self.node_type),
            )
            .with_detail("node_type", self.node_type.clone()));
        }
        if let Some(location) = self.location {
            location.check_finite("location")?;
        }
        for property in &self.properties {
            property.validate()?;
        }
        for input in &self.inputs {
            input.validate()?;
        }
        Ok(())
    }
}

/// A default value for an unconnected input socket.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SocketDefault {
    #[serde(flatten)]
    pub socket: SocketSelector,
    pub value: PropertyValue,
}

impl Validate for SocketDefault {
    fn validate(&self) -> Result<()> {
        if let SocketSelector::Identifier(id) | SocketSelector::Name(id) = &self.socket
            && id.is_empty()
        {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeSocket,
                "Socket identifier must not be empty.",
            ));
        }
        self.value.check_finite("socket default")
    }
}

/// `*.node.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateNode {
    #[serde(flatten)]
    pub tree: TreeTarget,
    pub node: NodeRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<SocketDefault>,
}

impl Validate for UpdateNode {
    fn validate(&self) -> Result<()> {
        if let Some(location) = self.location {
            location.check_finite("location")?;
        }
        for property in &self.properties {
            property.validate()?;
        }
        for input in &self.inputs {
            input.validate()?;
        }
        if self.name.is_none()
            && self.location.is_none()
            && self.mute.is_none()
            && self.properties.is_empty()
            && self.inputs.is_empty()
        {
            return Err(BlenderError::invalid_argument(
                "`node.update` needs at least one change.",
            ));
        }
        Ok(())
    }
}

/// `*.link.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateLink {
    #[serde(flatten)]
    pub tree: TreeTarget,
    /// Source socket. Must be an output.
    pub from: SocketAddress,
    /// Destination socket. Must be an input.
    pub to: SocketAddress,
    /// Replace any existing link into the destination. Blender allows only one
    /// link per input socket and silently drops the old one; making that
    /// explicit stops callers from losing a connection by accident.
    #[serde(default = "crate::object::default_true")]
    pub replace_existing: bool,
}

impl Validate for CreateLink {
    fn validate(&self) -> Result<()> {
        if self.from.direction != SocketDirection::Output {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeSocket,
                "`from` must address an output socket.",
            )
            .with_detail("direction", "input"));
        }
        if self.to.direction != SocketDirection::Input {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeSocket,
                "`to` must address an input socket.",
            )
            .with_detail("direction", "output"));
        }
        if self.from.node == self.to.node {
            return Err(BlenderError::new(
                ErrorCode::InvalidNodeSocket,
                "A node cannot be linked to itself.",
            )
            .with_detail("node", self.from.node.to_string()));
        }
        Ok(())
    }
}

/// `*.link.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteLink {
    #[serde(flatten)]
    pub tree: TreeTarget,
    /// Delete the link feeding this input socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<SocketAddress>,
    /// Delete every link leaving this output socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<SocketAddress>,
}

impl Validate for DeleteLink {
    fn validate(&self) -> Result<()> {
        if self.to.is_none() && self.from.is_none() {
            return Err(BlenderError::invalid_argument(
                "Specify `to`, `from`, or both.",
            ));
        }
        Ok(())
    }
}

/// `*.tree.get` -- controls how much of the graph comes back.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetTree {
    #[serde(flatten)]
    pub tree: TreeTarget,
    /// Include each socket's current default value.
    #[serde(default)]
    pub include_socket_defaults: bool,
    /// Include node locations, labels, colours and widths.
    #[serde(default)]
    pub include_ui_metadata: bool,
    /// Include node properties beyond the identifying ones.
    #[serde(default)]
    pub include_properties: bool,
    /// Only return these nodes and the links between them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeRef>,
}

impl Default for GetTree {
    fn default() -> Self {
        Self {
            tree: TreeTarget::World,
            include_socket_defaults: false,
            include_ui_metadata: false,
            include_properties: false,
            nodes: Vec::new(),
        }
    }
}

/// A node as reported by `*.tree.get` / `*.node.list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeSummary {
    pub id: NodeId,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Vec2>,
    #[serde(default)]
    pub mute: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<SocketSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<SocketSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAssignment>,
}

/// A socket as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SocketSummary {
    pub identifier: String,
    pub name: String,
    pub index: u32,
    /// Blender socket type, e.g. `VALUE`, `RGBA`, `VECTOR`, `SHADER`.
    #[serde(rename = "type")]
    pub socket_type: String,
    #[serde(default)]
    pub is_linked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<PropertyValue>,
}

/// A link as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinkSummary {
    pub from_node: NodeId,
    pub from_socket: String,
    pub to_node: NodeId,
    pub to_socket: String,
    #[serde(default)]
    pub is_valid: bool,
}

/// A whole graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphSummary {
    pub domain: GraphDomain,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<NodeSummary>,
    #[serde(default)]
    pub links: Vec<LinkSummary>,
}

impl GraphSummary {
    /// Find a node by id.
    pub fn node(&self, id: NodeId) -> Option<&NodeSummary> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Nodes with no path to an output node, which is the usual reason a
    /// hand-built graph renders as flat grey.
    pub fn dangling_nodes(&self) -> Vec<&NodeSummary> {
        let mut reaches_output = std::collections::BTreeSet::new();
        let outputs: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.node_type.contains("Output"))
            .map(|n| n.id)
            .collect();
        let mut frontier = outputs.clone();
        reaches_output.extend(outputs);
        while let Some(current) = frontier.pop() {
            for link in self.links.iter().filter(|l| l.to_node == current) {
                if reaches_output.insert(link.from_node) {
                    frontier.push(link.from_node);
                }
            }
        }
        self.nodes
            .iter()
            .filter(|n| !reaches_output.contains(&n.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(node: &str, direction: SocketDirection) -> SocketAddress {
        SocketAddress {
            node: NodeRef::name(node),
            socket: SocketSelector::Identifier("Color".into()),
            direction,
        }
    }

    #[test]
    fn links_must_run_output_to_input() {
        let bad = CreateLink {
            tree: TreeTarget::World,
            from: addr("A", SocketDirection::Input),
            to: addr("B", SocketDirection::Input),
            replace_existing: true,
        };
        assert_eq!(
            bad.validate().unwrap_err().code,
            ErrorCode::InvalidNodeSocket
        );

        let good = CreateLink {
            tree: TreeTarget::World,
            from: addr("A", SocketDirection::Output),
            to: addr("B", SocketDirection::Input),
            replace_existing: true,
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn self_links_are_rejected() {
        let params = CreateLink {
            tree: TreeTarget::World,
            from: addr("A", SocketDirection::Output),
            to: addr("A", SocketDirection::Input),
            replace_existing: true,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn property_names_must_be_identifiers() {
        let bad = PropertyAssignment {
            name: "operation; import os".into(),
            value: PropertyValue::Enum("ADD".into()),
        };
        assert_eq!(bad.validate().unwrap_err().code, ErrorCode::InvalidProperty);

        let dunder = PropertyAssignment {
            name: "__class__".into(),
            value: PropertyValue::Enum("x".into()),
        };
        assert_eq!(
            dunder.validate().unwrap_err().code,
            ErrorCode::PermissionDenied
        );

        let good = PropertyAssignment {
            name: "blend_type".into(),
            value: PropertyValue::Enum("MULTIPLY".into()),
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn non_finite_property_values_are_rejected() {
        let bad = PropertyAssignment {
            name: "value".into(),
            value: PropertyValue::Float(f64::INFINITY),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn dangling_nodes_are_those_that_never_reach_an_output() {
        let connected = NodeId::new();
        let output = NodeId::new();
        let orphan = NodeId::new();
        let graph = GraphSummary {
            domain: GraphDomain::Shader,
            name: "Material".into(),
            nodes: vec![
                NodeSummary {
                    id: connected,
                    name: "BSDF".into(),
                    node_type: "ShaderNodeBsdfPrincipled".into(),
                    label: None,
                    location: None,
                    mute: false,
                    inputs: vec![],
                    outputs: vec![],
                    properties: vec![],
                },
                NodeSummary {
                    id: output,
                    name: "Output".into(),
                    node_type: "ShaderNodeOutputMaterial".into(),
                    label: None,
                    location: None,
                    mute: false,
                    inputs: vec![],
                    outputs: vec![],
                    properties: vec![],
                },
                NodeSummary {
                    id: orphan,
                    name: "Noise".into(),
                    node_type: "ShaderNodeTexNoise".into(),
                    label: None,
                    location: None,
                    mute: false,
                    inputs: vec![],
                    outputs: vec![],
                    properties: vec![],
                },
            ],
            links: vec![LinkSummary {
                from_node: connected,
                from_socket: "BSDF".into(),
                to_node: output,
                to_socket: "Surface".into(),
                is_valid: true,
            }],
        };
        let dangling: Vec<_> = graph.dangling_nodes().iter().map(|n| n.id).collect();
        assert_eq!(dangling, vec![orphan]);
    }
}

/// `*.node.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteNode {
    #[serde(flatten)]
    pub tree: TreeTarget,
    pub node: NodeRef,
    /// Delete even the tree output node, which leaves the material black.
    #[serde(default)]
    pub force: bool,
}

/// `*.node.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetNode {
    #[serde(flatten)]
    pub tree: TreeTarget,
    pub node: NodeRef,
}

/// `*.node.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListNodes {
    #[serde(flatten)]
    pub tree: TreeTarget,
    #[serde(default, flatten)]
    pub page: crate::Page,
}

/// `*.link.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListLinks {
    #[serde(flatten)]
    pub tree: TreeTarget,
}

/// `*.tree.clear`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClearTree {
    #[serde(flatten)]
    pub tree: TreeTarget,
    /// Keep the output node so the tree can be rebuilt onto it.
    #[serde(default = "crate::object::default_true")]
    pub keep_output: bool,
}

/// `*.socket.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetSocket {
    #[serde(flatten)]
    pub tree: TreeTarget,
    pub node: NodeRef,
    #[serde(flatten)]
    pub socket: SocketSelector,
    #[serde(default = "default_input_direction")]
    pub direction: SocketDirection,
}

fn default_input_direction() -> SocketDirection {
    SocketDirection::Input
}

/// `*.socket.set_default`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSocketDefault {
    #[serde(flatten)]
    pub tree: TreeTarget,
    pub node: NodeRef,
    #[serde(flatten)]
    pub socket: SocketSelector,
    pub value: PropertyValue,
    /// Write the default even though a link already drives the socket, where
    /// it will have no visible effect.
    #[serde(default)]
    pub force: bool,
}

impl Validate for DeleteNode {}
impl Validate for GetNode {}
impl Validate for ListLinks {}
impl Validate for ClearTree {}
impl Validate for GetSocket {}

impl Validate for ListNodes {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

impl Validate for SetSocketDefault {
    fn validate(&self) -> Result<()> {
        self.value.check_finite("value")
    }
}

impl Validate for GetTree {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

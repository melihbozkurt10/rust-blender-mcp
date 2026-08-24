//! Geometry node payloads.
//!
//! Node and link editing reuses [`crate::node_graph`] wholesale -- a geometry
//! node tree is a node tree. What is genuinely different lives here: node group
//! lifecycle, the *interface* (the group's own inputs and outputs, which is a
//! separate API from its sockets), modifier attachment, and the scatter /
//! array-along-curve planners.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BlenderError, Page, Result, Validate, check_name,
    ids::{CollectionRef, NodeTreeId, NodeTreeRef, ObjectRef},
    math::{Axis, Vec3, check_non_negative, check_positive, check_range},
};

/// Socket types a group interface can expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InterfaceSocketType {
    Geometry,
    Float,
    Int,
    Bool,
    Vector,
    Color,
    String,
    Object,
    Collection,
    Material,
    Image,
    Rotation,
    Menu,
}

impl InterfaceSocketType {
    /// The `bl_socket_idname` Blender expects for this type.
    pub const fn socket_idname(self) -> &'static str {
        match self {
            InterfaceSocketType::Geometry => "NodeSocketGeometry",
            InterfaceSocketType::Float => "NodeSocketFloat",
            InterfaceSocketType::Int => "NodeSocketInt",
            InterfaceSocketType::Bool => "NodeSocketBool",
            InterfaceSocketType::Vector => "NodeSocketVector",
            InterfaceSocketType::Color => "NodeSocketColor",
            InterfaceSocketType::String => "NodeSocketString",
            InterfaceSocketType::Object => "NodeSocketObject",
            InterfaceSocketType::Collection => "NodeSocketCollection",
            InterfaceSocketType::Material => "NodeSocketMaterial",
            InterfaceSocketType::Image => "NodeSocketImage",
            InterfaceSocketType::Rotation => "NodeSocketRotation",
            InterfaceSocketType::Menu => "NodeSocketMenu",
        }
    }

    /// Whether min/max/default bounds are meaningful for this type.
    pub const fn is_numeric(self) -> bool {
        matches!(
            self,
            InterfaceSocketType::Float | InterfaceSocketType::Int | InterfaceSocketType::Vector
        )
    }
}

/// `geometry_nodes.group.create`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateNodeGroup {
    pub name: String,
    /// Add the standard Group Input -> Group Output geometry pass-through.
    #[serde(default = "crate::object::default_true")]
    pub with_geometry_io: bool,
    /// Attach the new group to this object as a Nodes modifier straight away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<ObjectRef>,
}

impl Validate for CreateNodeGroup {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")
    }
}

/// `geometry_nodes.interface.add_socket`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddInterfaceSocket {
    pub group: NodeTreeRef,
    pub name: String,
    #[serde(rename = "type")]
    pub socket_type: InterfaceSocketType,
    #[serde(default = "default_input")]
    pub direction: crate::node_graph::SocketDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<crate::node_graph::PropertyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Position among the group's existing sockets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

fn default_input() -> crate::node_graph::SocketDirection {
    crate::node_graph::SocketDirection::Input
}

impl Validate for AddInterfaceSocket {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")?;
        if let (Some(min), Some(max)) = (self.min, self.max)
            && min > max
        {
            return Err(BlenderError::invalid_argument(format!(
                "`min` ({min}) is greater than `max` ({max})."
            )));
        }
        if (self.min.is_some() || self.max.is_some()) && !self.socket_type.is_numeric() {
            return Err(BlenderError::invalid_argument(format!(
                "`min`/`max` are meaningless for a {:?} socket.",
                self.socket_type
            ))
            .with_detail("socket_type", self.socket_type.socket_idname()));
        }
        Ok(())
    }
}

/// `geometry_nodes.modifier.attach`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttachNodeGroup {
    pub object: ObjectRef,
    pub group: NodeTreeRef,
    /// Modifier name. Defaults to the group's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_name: Option<String>,
    /// Values for the group's exposed inputs, by interface socket name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InterfaceValue>,
}

/// A value for one exposed group input.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InterfaceValue {
    /// Interface socket name as shown in the modifier panel.
    pub name: String,
    pub value: crate::node_graph::PropertyValue,
}

impl Validate for InterfaceValue {
    fn validate(&self) -> Result<()> {
        check_name(&self.name, "name")?;
        crate::math::Finite::check_finite(&self.value, &format!("input `{}`", self.name))
    }
}

impl Validate for AttachNodeGroup {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.modifier_name {
            check_name(name, "modifier_name")?;
        }
        for input in &self.inputs {
            input.validate()?;
        }
        Ok(())
    }
}

/// What gets instanced by a scatter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScatterSource {
    /// Instance a single object.
    Object(ObjectRef),
    /// Pick randomly from a collection.
    Collection(CollectionRef),
}

/// `geometry_nodes.scatter` -- distribute instances over a surface.
///
/// The whole graph is planned in Rust: which nodes, which links, which
/// defaults. Blender receives a list of node and link creations, not a
/// description of intent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Scatter {
    /// Mesh whose surface receives the instances.
    pub surface: ObjectRef,
    pub source: ScatterSource,
    /// Instances per square unit of surface area.
    #[serde(default = "default_density")]
    pub density: f64,
    #[serde(default = "default_seed")]
    pub seed: i32,
    /// Uniform scale range applied per instance.
    #[serde(default = "default_scale_min")]
    pub scale_min: f64,
    #[serde(default = "default_scale_max")]
    pub scale_max: f64,
    /// Maximum random rotation about each axis, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_jitter: Option<Vec3>,
    /// Align instances to the surface normal rather than world Z.
    #[serde(default = "crate::object::default_true")]
    pub align_to_normal: bool,
    /// Vertex group or attribute name restricting where instances land.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density_attribute: Option<String>,
    /// Minimum distance between instances. Switches the distribution from
    /// random to Poisson disk, which is slower but does not clump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_distance: Option<f64>,
    /// Convert instances to real geometry. Costly, but required by most
    /// exporters.
    #[serde(default)]
    pub realize_instances: bool,
    /// Name for the generated node group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_density() -> f64 {
    10.0
}
fn default_seed() -> i32 {
    0
}
fn default_scale_min() -> f64 {
    1.0
}
fn default_scale_max() -> f64 {
    1.0
}

impl Validate for Scatter {
    fn validate(&self) -> Result<()> {
        check_positive(self.density, "density")?;
        check_positive(self.scale_min, "scale_min")?;
        check_positive(self.scale_max, "scale_max")?;
        if self.scale_min > self.scale_max {
            return Err(BlenderError::invalid_argument(format!(
                "`scale_min` ({}) exceeds `scale_max` ({}).",
                self.scale_min, self.scale_max
            )));
        }
        if let Some(jitter) = self.rotation_jitter {
            crate::math::Finite::check_finite(&jitter, "rotation_jitter")?;
        }
        if let Some(distance) = self.minimum_distance {
            check_non_negative(distance, "minimum_distance")?;
        }
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if let Some(attribute) = &self.density_attribute {
            check_name(attribute, "density_attribute")?;
        }
        Ok(())
    }
}

/// How instances are spaced along a curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveSpacing {
    /// A fixed number of instances, evenly spread.
    Count(u32),
    /// A fixed distance between instances.
    Spacing(f64),
}

/// `geometry_nodes.array_along_curve`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArrayAlongCurve {
    /// Object to instance.
    pub source: ObjectRef,
    /// Curve object to follow.
    pub curve: ObjectRef,
    #[serde(flatten)]
    pub spacing: CurveSpacing,
    /// Which local axis points along the curve tangent.
    #[serde(default = "default_forward_axis")]
    pub align_axis: Axis,
    /// Offset applied to each instance, in the curve's local space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<Vec3>,
    /// Rotate instances to follow the curve rather than keeping world rotation.
    #[serde(default = "crate::object::default_true")]
    pub follow_curve: bool,
    #[serde(default)]
    pub realize_instances: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_forward_axis() -> Axis {
    Axis::Y
}

impl Validate for ArrayAlongCurve {
    fn validate(&self) -> Result<()> {
        match self.spacing {
            CurveSpacing::Count(count) => {
                if count == 0 {
                    return Err(BlenderError::invalid_argument(
                        "`count` must be at least 1.",
                    ));
                }
                if count > 100_000 {
                    return Err(BlenderError::invalid_argument(format!(
                        "`count` of {count} would generate more instances than Blender handles interactively."
                    )));
                }
            }
            CurveSpacing::Spacing(spacing) => check_positive(spacing, "spacing")?,
        }
        if let Some(offset) = self.offset {
            crate::math::Finite::check_finite(&offset, "offset")?;
        }
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if self.source == self.curve {
            return Err(BlenderError::invalid_argument(
                "`source` and `curve` are the same object.",
            ));
        }
        Ok(())
    }
}

/// `geometry_nodes.group.list` filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListNodeGroups {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only groups attached to this object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_by: Option<ObjectRef>,
    #[serde(default, flatten)]
    pub page: Page,
}

/// A geometry node group as reported by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeGroupSummary {
    pub id: NodeTreeId,
    pub name: String,
    #[serde(default)]
    pub node_count: u32,
    #[serde(default)]
    pub users: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InterfaceSocketSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<InterfaceSocketSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
}

/// One exposed group socket.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InterfaceSocketSummary {
    pub identifier: String,
    pub name: String,
    #[serde(rename = "type")]
    pub socket_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<crate::node_graph::PropertyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Clamp a scatter's expected instance count so a request cannot wedge Blender.
///
/// Returns the projected instance count for the given surface area.
pub fn projected_instance_count(density: f64, surface_area: f64) -> f64 {
    (density * surface_area).max(0.0)
}

/// Reject a scatter whose projected instance count is beyond what Blender can
/// evaluate interactively.
pub fn check_instance_budget(count: f64, budget: f64) -> Result<()> {
    if count > budget {
        return Err(BlenderError::invalid_argument(format!(
            "This scatter would create roughly {count:.0} instances, past the configured budget of {budget:.0}. Lower `density`, or raise the budget deliberately."
        ))
        .with_detail("projected_instances", count.round())
        .with_detail("budget", budget));
    }
    Ok(())
}

impl Validate for ListNodeGroups {
    fn validate(&self) -> Result<()> {
        self.page.validate()
    }
}

/// Shared helper for scatter validation that needs the surface area, which only
/// the bridge knows.
pub fn validate_scatter_against_area(
    scatter: &Scatter,
    surface_area: f64,
    budget: f64,
) -> Result<()> {
    scatter.validate()?;
    check_range(surface_area, 0.0, f64::MAX, "surface_area")?;
    check_instance_budget(
        projected_instance_count(scatter.density, surface_area),
        budget,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scatter() -> Scatter {
        Scatter {
            surface: ObjectRef::name("Ground"),
            source: ScatterSource::Object(ObjectRef::name("Rock")),
            density: 10.0,
            seed: 0,
            scale_min: 1.0,
            scale_max: 1.0,
            rotation_jitter: None,
            align_to_normal: true,
            density_attribute: None,
            minimum_distance: None,
            realize_instances: false,
            name: None,
        }
    }

    #[test]
    fn inverted_scale_range_is_rejected() {
        let params = Scatter {
            scale_min: 2.0,
            scale_max: 1.0,
            ..scatter()
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn instance_budget_is_enforced() {
        // 10 per square unit over 10 000 square units is 100 000 instances.
        let err = validate_scatter_against_area(&scatter(), 10_000.0, 50_000.0).unwrap_err();
        assert_eq!(err.details["budget"], 50_000.0);
        assert!(validate_scatter_against_area(&scatter(), 100.0, 50_000.0).is_ok());
    }

    #[test]
    fn interface_bounds_only_apply_to_numbers() {
        let params = AddInterfaceSocket {
            group: NodeTreeRef::name("Scatter"),
            name: "Target".into(),
            socket_type: InterfaceSocketType::Object,
            direction: crate::node_graph::SocketDirection::Input,
            default_value: None,
            min: Some(0.0),
            max: Some(1.0),
            description: None,
            index: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn array_rejects_self_referential_curve() {
        let params = ArrayAlongCurve {
            source: ObjectRef::name("Fence"),
            curve: ObjectRef::name("Fence"),
            spacing: CurveSpacing::Count(10),
            align_axis: Axis::Y,
            offset: None,
            follow_curve: true,
            realize_instances: false,
            name: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn socket_idnames_match_blender() {
        assert_eq!(
            InterfaceSocketType::Geometry.socket_idname(),
            "NodeSocketGeometry"
        );
        assert_eq!(
            InterfaceSocketType::Collection.socket_idname(),
            "NodeSocketCollection"
        );
    }
}

/// `geometry_nodes.group.get` / `geometry_nodes.interface.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GroupRefParams {
    pub group: NodeTreeRef,
}

/// `geometry_nodes.group.delete`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteNodeGroup {
    pub group: NodeTreeRef,
    /// Delete even while modifiers still use it.
    #[serde(default)]
    pub force: bool,
}

/// `geometry_nodes.interface.update_socket`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateInterfaceSocket {
    pub group: NodeTreeRef,
    /// Socket identifier or current name.
    pub socket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<crate::node_graph::PropertyValue>,
}

/// `geometry_nodes.interface.delete_socket`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteInterfaceSocket {
    pub group: NodeTreeRef,
    pub socket: String,
}

/// `geometry_nodes.modifier.detach` / `geometry_nodes.modifier.list`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GeometryModifierRef {
    pub object: ObjectRef,
    /// Modifier name. Omit when the object has exactly one nodes modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier: Option<String>,
}

/// One node in a declarative graph plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannedNode {
    /// Caller-chosen key, referenced by the links below.
    pub key: String,
    pub node_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<crate::math::Vec2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<crate::node_graph::PropertyAssignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<crate::node_graph::SocketDefault>,
}

/// One link in a declarative graph plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannedLink {
    pub from: PlannedSocket,
    pub to: PlannedSocket,
}

/// A socket reference inside a plan: a node key plus a selector.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannedSocket {
    /// Key of a node in the same plan, or a reference to an existing node.
    pub node: String,
    #[serde(flatten)]
    pub socket: crate::node_graph::SocketSelector,
}

/// `geometry_nodes.graph.build` -- apply a whole graph plan in one pass.
///
/// This is the operation the scatter and array workflows target: the server
/// works out which nodes to create and how to wire them, then sends the result
/// as data. Nothing about the plan is interpreted by the bridge beyond
/// creating and connecting what it names.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BuildGraph {
    #[serde(flatten)]
    pub tree: crate::node_graph::TreeTarget,
    /// Remove every existing node first.
    #[serde(default)]
    pub clear: bool,
    pub nodes: Vec<PlannedNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<PlannedLink>,
}

impl Validate for GroupRefParams {}
impl Validate for DeleteNodeGroup {}
impl Validate for GeometryModifierRef {}

impl Validate for DeleteInterfaceSocket {
    fn validate(&self) -> Result<()> {
        if self.socket.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`socket` must not be empty.",
            ));
        }
        Ok(())
    }
}

impl Validate for UpdateInterfaceSocket {
    fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            check_name(name, "name")?;
        }
        if let (Some(min), Some(max)) = (self.min, self.max)
            && min > max
        {
            return Err(BlenderError::invalid_argument(format!(
                "`min` ({min}) is greater than `max` ({max})."
            )));
        }
        if self.name.is_none()
            && self.min.is_none()
            && self.max.is_none()
            && self.description.is_none()
            && self.default_value.is_none()
        {
            return Err(BlenderError::invalid_argument("Nothing to update."));
        }
        Ok(())
    }
}

impl Validate for BuildGraph {
    fn validate(&self) -> Result<()> {
        if self.nodes.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`nodes` must contain at least one node.",
            ));
        }
        if self.nodes.len() > 500 {
            return Err(BlenderError::invalid_argument(format!(
                "{} nodes in one plan is beyond what a graph should need.",
                self.nodes.len()
            )));
        }

        let mut keys = std::collections::BTreeSet::new();
        for node in &self.nodes {
            if node.key.is_empty() {
                return Err(BlenderError::invalid_argument(
                    "Every planned node needs a non-empty `key`.",
                ));
            }
            if !keys.insert(node.key.as_str()) {
                return Err(BlenderError::invalid_argument(format!(
                    "Duplicate node key `{}`.",
                    node.key
                ))
                .with_detail("key", node.key.clone()));
            }
            if node.node_type.is_empty()
                || !node
                    .node_type
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(BlenderError::new(
                    crate::ErrorCode::InvalidNodeType,
                    format!("`{}` is not a valid node type identifier.", node.node_type),
                ));
            }
            for property in &node.properties {
                property.validate()?;
            }
            for input in &node.inputs {
                input.validate()?;
            }
        }

        // Links may only reference keys the plan defines. Referring to a node
        // that is not in the plan is almost always a typo, and catching it here
        // means the whole plan is rejected before half of it has been built.
        for link in &self.links {
            for (label, socket) in [("from", &link.from), ("to", &link.to)] {
                if !keys.contains(socket.node.as_str()) {
                    return Err(BlenderError::invalid_argument(format!(
                        "Link `{label}` names node `{}`, which the plan does not define.",
                        socket.node
                    ))
                    .with_detail("node", socket.node.clone())
                    .with_detail_json("defined", &keys.iter().copied().collect::<Vec<_>>()));
                }
            }
            if link.from.node == link.to.node {
                return Err(BlenderError::invalid_argument(format!(
                    "Node `{}` cannot be linked to itself.",
                    link.from.node
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::node_graph::SocketSelector;

    fn node(key: &str) -> PlannedNode {
        PlannedNode {
            key: key.to_string(),
            node_type: "GeometryNodeSetPosition".into(),
            name: None,
            location: None,
            properties: vec![],
            inputs: vec![],
        }
    }

    fn plan(nodes: Vec<PlannedNode>, links: Vec<PlannedLink>) -> BuildGraph {
        BuildGraph {
            tree: crate::node_graph::TreeTarget::NodeTree(crate::ids::NodeTreeRef::name("G")),
            clear: false,
            nodes,
            links,
        }
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        assert!(plan(vec![node("a"), node("a")], vec![]).validate().is_err());
    }

    #[test]
    fn links_must_name_planned_nodes() {
        let link = PlannedLink {
            from: PlannedSocket {
                node: "a".into(),
                socket: SocketSelector::Index(0),
            },
            to: PlannedSocket {
                node: "ghost".into(),
                socket: SocketSelector::Index(0),
            },
        };
        let error = plan(vec![node("a")], vec![link]).validate().unwrap_err();
        assert_eq!(error.details["node"], "ghost");
    }

    #[test]
    fn a_valid_plan_passes() {
        let link = PlannedLink {
            from: PlannedSocket {
                node: "a".into(),
                socket: SocketSelector::Index(0),
            },
            to: PlannedSocket {
                node: "b".into(),
                socket: SocketSelector::Index(0),
            },
        };
        assert!(
            plan(vec![node("a"), node("b")], vec![link])
                .validate()
                .is_ok()
        );
    }
}

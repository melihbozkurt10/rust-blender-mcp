//! Geometry node graph planning.
//!
//! Scattering objects over a surface and arraying them along a curve are both
//! fixed graph shapes with a handful of parameters. Building them here means
//! the same graph every time, one round trip to Blender, and a plan that can be
//! inspected before anything is created.

use blender_protocol::{
    BlenderError, Result,
    geometry_nodes::{
        ArrayAlongCurve, CurveSpacing, PlannedLink, PlannedNode, PlannedSocket, Scatter,
        ScatterSource,
    },
    math::{Vec2, Vec3},
    node_graph::{PropertyAssignment, PropertyValue, SocketDefault, SocketSelector},
};

use crate::material::GraphPlan;

const ROW: f64 = 260.0;

fn node(key: &str, node_type: &str, x: f64, y: f64) -> PlannedNode {
    PlannedNode {
        key: key.to_string(),
        node_type: node_type.to_string(),
        name: None,
        location: Some(Vec2::new(x, y)),
        properties: Vec::new(),
        inputs: Vec::new(),
    }
}

fn link(from: &str, from_socket: &str, to: &str, to_socket: &str) -> PlannedLink {
    PlannedLink {
        from: PlannedSocket {
            node: from.to_string(),
            socket: SocketSelector::Name(from_socket.to_string()),
        },
        to: PlannedSocket {
            node: to.to_string(),
            socket: SocketSelector::Name(to_socket.to_string()),
        },
    }
}

fn link_index(from: &str, from_index: u32, to: &str, to_index: u32) -> PlannedLink {
    PlannedLink {
        from: PlannedSocket {
            node: from.to_string(),
            socket: SocketSelector::Index(from_index),
        },
        to: PlannedSocket {
            node: to.to_string(),
            socket: SocketSelector::Index(to_index),
        },
    }
}

/// A link whose *source* is addressed by index.
///
/// The Random Value node exposes one output per data type and calls every one
/// of them `Value`, so a name is ambiguous and the bridge rightly refuses it.
/// The vector output is index 0.
fn link_from_index(from: &str, from_index: u32, to: &str, to_socket: &str) -> PlannedLink {
    PlannedLink {
        from: PlannedSocket {
            node: from.to_string(),
            socket: SocketSelector::Index(from_index),
        },
        to: PlannedSocket {
            node: to.to_string(),
            socket: SocketSelector::Name(to_socket.to_string()),
        },
    }
}

fn default_socket(name: &str, value: PropertyValue) -> SocketDefault {
    SocketDefault {
        socket: SocketSelector::Name(name.to_string()),
        value,
    }
}

/// Plan a scatter graph.
pub fn plan_scatter(spec: &Scatter) -> Result<GraphPlan> {
    blender_protocol::Validate::validate(spec)?;

    let mut nodes = vec![
        node("input", "NodeGroupInput", -900.0, 0.0),
        node(
            "distribute",
            "GeometryNodeDistributePointsOnFaces",
            -600.0,
            0.0,
        ),
        node("object_info", "GeometryNodeObjectInfo", -600.0, -ROW * 2.0),
        node("instance", "GeometryNodeInstanceOnPoints", -250.0, 0.0),
        node("output", "NodeGroupOutput", 400.0, 0.0),
    ];
    let mut links = vec![
        link_index("input", 0, "distribute", 0),
        link("distribute", "Points", "instance", "Points"),
    ];

    // Poisson-disk distribution costs more but does not clump, which is what
    // anyone scattering rocks or grass actually wants.
    let distribute = nodes.iter_mut().find(|n| n.key == "distribute").unwrap();
    if let Some(distance) = spec.minimum_distance {
        distribute.properties.push(PropertyAssignment {
            name: "distribute_method".into(),
            value: PropertyValue::Enum("POISSON".into()),
        });
        distribute.inputs.push(default_socket(
            "Distance Min",
            PropertyValue::Float(distance),
        ));
        distribute.inputs.push(default_socket(
            "Density Max",
            PropertyValue::Float(spec.density),
        ));
    } else {
        distribute.inputs.push(default_socket(
            "Density",
            PropertyValue::Float(spec.density),
        ));
    }
    distribute
        .inputs
        .push(default_socket("Seed", PropertyValue::Int(spec.seed as i64)));

    // What gets instanced: one object, or a random pick from a collection.
    match &spec.source {
        ScatterSource::Object(object) => {
            let info = nodes.iter_mut().find(|n| n.key == "object_info").unwrap();
            info.properties.push(PropertyAssignment {
                name: "transform_space".into(),
                value: PropertyValue::Enum("RELATIVE".into()),
            });
            info.inputs.push(SocketDefault {
                socket: SocketSelector::Name("Object".into()),
                value: PropertyValue::Object(object.clone()),
            });
            links.push(link("object_info", "Geometry", "instance", "Instance"));
        }
        ScatterSource::Collection(collection) => {
            nodes.retain(|n| n.key != "object_info");
            let mut info = node(
                "collection_info",
                "GeometryNodeCollectionInfo",
                -600.0,
                -ROW * 2.0,
            );
            info.properties.push(PropertyAssignment {
                name: "transform_space".into(),
                value: PropertyValue::Enum("RELATIVE".into()),
            });
            info.inputs.push(SocketDefault {
                socket: SocketSelector::Name("Collection".into()),
                value: PropertyValue::Collection(collection.clone()),
            });
            info.inputs.push(default_socket(
                "Separate Children",
                PropertyValue::Bool(true),
            ));
            nodes.push(info);

            // `Separate Children` turns the collection into one instance per
            // child, and `Pick Instance` makes each point choose one at
            // random. Without that pair every point gets the same object and
            // the scatter looks like a stamp, not scattered rocks.
            let instance = nodes.iter_mut().find(|n| n.key == "instance").unwrap();
            instance
                .inputs
                .push(default_socket("Pick Instance", PropertyValue::Bool(true)));
            links.push(link("collection_info", "Instances", "instance", "Instance"));
        }
    }

    // Scale and rotation randomisation.
    if spec.scale_min != spec.scale_max {
        let mut random = node("random_scale", "FunctionNodeRandomValue", -600.0, ROW);
        random.properties.push(PropertyAssignment {
            name: "data_type".into(),
            value: PropertyValue::Enum("FLOAT_VECTOR".into()),
        });
        random.inputs.push(SocketDefault {
            socket: SocketSelector::Index(0),
            value: PropertyValue::Vec3(Vec3::splat(spec.scale_min)),
        });
        random.inputs.push(SocketDefault {
            socket: SocketSelector::Index(1),
            value: PropertyValue::Vec3(Vec3::splat(spec.scale_max)),
        });
        nodes.push(random);
        links.push(link_from_index("random_scale", 0, "instance", "Scale"));
    } else if (spec.scale_min - 1.0).abs() > f64::EPSILON {
        let instance = nodes.iter_mut().find(|n| n.key == "instance").unwrap();
        instance.inputs.push(default_socket(
            "Scale",
            PropertyValue::Vec3(Vec3::splat(spec.scale_min)),
        ));
    }

    if let Some(jitter) = spec.rotation_jitter {
        let mut random = node(
            "random_rotation",
            "FunctionNodeRandomValue",
            -600.0,
            ROW * 2.0,
        );
        random.properties.push(PropertyAssignment {
            name: "data_type".into(),
            value: PropertyValue::Enum("FLOAT_VECTOR".into()),
        });
        let radians = Vec3::new(
            jitter.x.to_radians(),
            jitter.y.to_radians(),
            jitter.z.to_radians(),
        );
        random.inputs.push(SocketDefault {
            socket: SocketSelector::Index(0),
            value: PropertyValue::Vec3(Vec3::new(-radians.x, -radians.y, -radians.z)),
        });
        random.inputs.push(SocketDefault {
            socket: SocketSelector::Index(1),
            value: PropertyValue::Vec3(radians),
        });
        nodes.push(random);
        links.push(link_from_index(
            "random_rotation",
            0,
            "instance",
            "Rotation",
        ));
    } else if spec.align_to_normal {
        // Aligning to the surface normal is what the distribute node's own
        // rotation output is for.
        links.push(link("distribute", "Rotation", "instance", "Rotation"));
    }

    // Realising instances is expensive but is what exporters need.
    if spec.realize_instances {
        nodes.push(node("realize", "GeometryNodeRealizeInstances", 100.0, 0.0));
        links.retain(|l| !(l.from.node == "instance" && l.to.node == "output"));
        links.push(link("instance", "Instances", "realize", "Geometry"));
        links.push(link_index("realize", 0, "output", 0));
    } else if !links.iter().any(|l| l.to.node == "output") {
        links.push(link_index("instance", 0, "output", 0));
    }

    Ok(GraphPlan { nodes, links })
}

/// Plan an array-along-curve graph.
pub fn plan_array_along_curve(spec: &ArrayAlongCurve) -> Result<GraphPlan> {
    blender_protocol::Validate::validate(spec)?;

    let mut nodes = vec![
        node("curve_info", "GeometryNodeObjectInfo", -900.0, 0.0),
        node("resample", "GeometryNodeResampleCurve", -600.0, 0.0),
        node("to_points", "GeometryNodeCurveToPoints", -400.0, 0.0),
        node("source_info", "GeometryNodeObjectInfo", -600.0, -ROW * 2.0),
        node("instance", "GeometryNodeInstanceOnPoints", -150.0, 0.0),
        node("output", "NodeGroupOutput", 400.0, 0.0),
    ];

    let curve_info = nodes.iter_mut().find(|n| n.key == "curve_info").unwrap();
    curve_info.properties.push(PropertyAssignment {
        name: "transform_space".into(),
        value: PropertyValue::Enum("RELATIVE".into()),
    });
    curve_info.inputs.push(SocketDefault {
        socket: SocketSelector::Name("Object".into()),
        value: PropertyValue::Object(spec.curve.clone()),
    });

    let resample = nodes.iter_mut().find(|n| n.key == "resample").unwrap();
    match spec.spacing {
        CurveSpacing::Count(count) => {
            resample.properties.push(PropertyAssignment {
                name: "mode".into(),
                value: PropertyValue::Enum("COUNT".into()),
            });
            resample
                .inputs
                .push(default_socket("Count", PropertyValue::Int(count as i64)));
        }
        CurveSpacing::Spacing(spacing) => {
            resample.properties.push(PropertyAssignment {
                name: "mode".into(),
                value: PropertyValue::Enum("LENGTH".into()),
            });
            resample
                .inputs
                .push(default_socket("Length", PropertyValue::Float(spacing)));
        }
    }

    let source_info = nodes.iter_mut().find(|n| n.key == "source_info").unwrap();
    source_info.properties.push(PropertyAssignment {
        name: "transform_space".into(),
        value: PropertyValue::Enum("RELATIVE".into()),
    });
    source_info.inputs.push(SocketDefault {
        socket: SocketSelector::Name("Object".into()),
        value: PropertyValue::Object(spec.source.clone()),
    });

    let mut links = vec![
        link("curve_info", "Geometry", "resample", "Curve"),
        link("resample", "Curve", "to_points", "Curve"),
        link("to_points", "Points", "instance", "Points"),
        link("source_info", "Geometry", "instance", "Instance"),
    ];

    if spec.follow_curve {
        // The curve-to-points node hands out the tangent and normal, which is
        // what makes instances lie along the curve rather than all facing the
        // same way.
        nodes.push(node(
            "align",
            "FunctionNodeAlignRotationToVector",
            -250.0,
            ROW * 2.0,
        ));
        let align = nodes.iter_mut().find(|n| n.key == "align").unwrap();
        align.properties.push(PropertyAssignment {
            name: "axis".into(),
            value: PropertyValue::Enum(spec.align_axis.letter().to_string()),
        });
        links.push(link("to_points", "Tangent", "align", "Vector"));
        links.push(link("align", "Rotation", "instance", "Rotation"));
    }

    if let Some(offset) = spec.offset
        && offset.length() > 0.0
    {
        nodes.push(node("offset", "GeometryNodeTranslateInstances", 100.0, 0.0));
        let translate = nodes.iter_mut().find(|n| n.key == "offset").unwrap();
        translate
            .inputs
            .push(default_socket("Translation", PropertyValue::Vec3(offset)));
        links.push(link("instance", "Instances", "offset", "Instances"));
        if spec.realize_instances {
            nodes.push(node("realize", "GeometryNodeRealizeInstances", 250.0, 0.0));
            links.push(link("offset", "Instances", "realize", "Geometry"));
            links.push(link_index("realize", 0, "output", 0));
        } else {
            links.push(link_index("offset", 0, "output", 0));
        }
    } else if spec.realize_instances {
        nodes.push(node("realize", "GeometryNodeRealizeInstances", 150.0, 0.0));
        links.push(link("instance", "Instances", "realize", "Geometry"));
        links.push(link_index("realize", 0, "output", 0));
    } else {
        links.push(link_index("instance", 0, "output", 0));
    }

    Ok(GraphPlan { nodes, links })
}

/// How many instances a scatter is likely to produce over a given area.
pub fn projected_instances(density: f64, surface_area: f64) -> f64 {
    (density * surface_area).max(0.0)
}

/// Refuse a scatter that would bring Blender to its knees.
pub fn check_instance_budget(projected: f64, budget: f64) -> Result<()> {
    if projected > budget {
        return Err(BlenderError::invalid_argument(format!(
            "This scatter would create roughly {projected:.0} instances, past the budget of \
             {budget:.0}. Lower the density, restrict it with a density attribute, or raise the \
             budget deliberately."
        ))
        .with_detail("projected_instances", projected.round())
        .with_detail("budget", budget));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use blender_protocol::ids::{CollectionRef, ObjectRef};

    use super::*;

    fn scatter() -> Scatter {
        Scatter {
            surface: ObjectRef::name("Ground"),
            source: ScatterSource::Object(ObjectRef::name("Rock")),
            density: 10.0,
            seed: 3,
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

    fn array() -> ArrayAlongCurve {
        ArrayAlongCurve {
            source: ObjectRef::name("Post"),
            curve: ObjectRef::name("Path"),
            spacing: CurveSpacing::Count(12),
            align_axis: blender_protocol::math::Axis::Y,
            offset: None,
            follow_curve: true,
            realize_instances: false,
            name: None,
        }
    }

    #[test]
    fn a_scatter_wires_distribution_into_instancing() {
        let plan = plan_scatter(&scatter()).unwrap();
        assert!(plan.has_node("distribute") && plan.has_node("instance"));
        assert!(plan.link_between("distribute", "instance"));
        assert!(plan.link_between("instance", "output"));
    }

    #[test]
    fn a_minimum_distance_switches_to_poisson() {
        let mut spec = scatter();
        spec.minimum_distance = Some(0.5);
        let plan = plan_scatter(&spec).unwrap();
        let distribute = plan.nodes.iter().find(|n| n.key == "distribute").unwrap();
        assert!(distribute.properties.iter().any(|p| matches!(
            &p.value, PropertyValue::Enum(v) if v == "POISSON"
        )));
    }

    #[test]
    fn a_scale_range_adds_a_random_value_node() {
        let mut spec = scatter();
        spec.scale_min = 0.8;
        spec.scale_max = 1.4;
        let plan = plan_scatter(&spec).unwrap();
        assert!(plan.has_node("random_scale"));
        assert!(plan.link_between("random_scale", "instance"));
    }

    #[test]
    fn a_fixed_scale_needs_no_random_node() {
        let mut spec = scatter();
        spec.scale_min = 2.0;
        spec.scale_max = 2.0;
        let plan = plan_scatter(&spec).unwrap();
        assert!(!plan.has_node("random_scale"));
        let instance = plan.nodes.iter().find(|n| n.key == "instance").unwrap();
        assert!(
            !instance.inputs.is_empty(),
            "the fixed scale should be a socket default"
        );
    }

    #[test]
    fn aligning_to_the_normal_uses_the_distribution_rotation() {
        let plan = plan_scatter(&scatter()).unwrap();
        assert!(plan.links.iter().any(|l| l.from.node == "distribute"
            && l.to.node == "instance"
            && matches!(&l.from.socket, SocketSelector::Name(n) if n == "Rotation")));
    }

    #[test]
    fn realising_instances_inserts_a_realize_node_before_the_output() {
        let mut spec = scatter();
        spec.realize_instances = true;
        let plan = plan_scatter(&spec).unwrap();
        assert!(plan.has_node("realize"));
        assert!(plan.link_between("instance", "realize"));
        assert!(plan.link_between("realize", "output"));
        assert!(
            !plan.link_between("instance", "output"),
            "the direct link must be replaced, not duplicated"
        );
    }

    #[test]
    fn a_collection_source_uses_collection_info() {
        let mut spec = scatter();
        spec.source = ScatterSource::Collection(CollectionRef::name("Rocks"));
        let plan = plan_scatter(&spec).unwrap();
        assert!(plan.has_node("collection_info"));
        assert!(!plan.has_node("object_info"));
    }

    #[test]
    fn an_array_by_count_resamples_by_count() {
        let plan = plan_array_along_curve(&array()).unwrap();
        let resample = plan.nodes.iter().find(|n| n.key == "resample").unwrap();
        assert!(resample.properties.iter().any(|p| matches!(
            &p.value, PropertyValue::Enum(v) if v == "COUNT"
        )));
    }

    #[test]
    fn an_array_by_spacing_resamples_by_length() {
        let mut spec = array();
        spec.spacing = CurveSpacing::Spacing(1.5);
        let plan = plan_array_along_curve(&spec).unwrap();
        let resample = plan.nodes.iter().find(|n| n.key == "resample").unwrap();
        assert!(resample.properties.iter().any(|p| matches!(
            &p.value, PropertyValue::Enum(v) if v == "LENGTH"
        )));
    }

    #[test]
    fn following_the_curve_aligns_rotation_to_the_tangent() {
        let plan = plan_array_along_curve(&array()).unwrap();
        assert!(plan.has_node("align"));
        assert!(plan.link_between("to_points", "align"));
        assert!(plan.link_between("align", "instance"));
    }

    #[test]
    fn not_following_the_curve_leaves_rotation_alone() {
        let mut spec = array();
        spec.follow_curve = false;
        let plan = plan_array_along_curve(&spec).unwrap();
        assert!(!plan.has_node("align"));
    }

    #[test]
    fn an_offset_inserts_a_translate_node() {
        let mut spec = array();
        spec.offset = Some(Vec3::new(0.0, 0.0, 1.0));
        let plan = plan_array_along_curve(&spec).unwrap();
        assert!(plan.has_node("offset"));
        assert!(plan.link_between("instance", "offset"));
        assert!(plan.link_between("offset", "output"));
    }

    #[test]
    fn every_plan_reaches_the_output_exactly_once() {
        for plan in [
            plan_scatter(&scatter()).unwrap(),
            plan_array_along_curve(&array()).unwrap(),
        ] {
            let to_output = plan.links.iter().filter(|l| l.to.node == "output").count();
            assert_eq!(to_output, 1, "a graph with two output links is ambiguous");
        }
    }

    #[test]
    fn random_value_outputs_are_addressed_by_index() {
        // Every output of Random Value is called `Value`, so linking by name is
        // ambiguous and Blender refuses it. This caught a real failure.
        let mut spec = scatter();
        spec.scale_min = 0.5;
        spec.scale_max = 2.0;
        spec.rotation_jitter = Some(Vec3::new(0.0, 0.0, 180.0));
        let plan = plan_scatter(&spec).unwrap();

        for key in ["random_scale", "random_rotation"] {
            let link = plan
                .links
                .iter()
                .find(|l| l.from.node == key)
                .unwrap_or_else(|| panic!("no link from {key}"));
            assert!(
                matches!(link.from.socket, SocketSelector::Index(_)),
                "`{key}` must be linked by index, got {:?}",
                link.from.socket
            );
        }
    }

    #[test]
    fn the_instance_budget_is_enforced() {
        assert!(check_instance_budget(projected_instances(10.0, 100.0), 5000.0).is_ok());
        let error = check_instance_budget(projected_instances(10.0, 10_000.0), 5000.0).unwrap_err();
        assert_eq!(error.details["budget"], 5000.0);
    }

    #[test]
    fn an_invalid_spec_is_refused_before_planning() {
        let mut spec = scatter();
        spec.scale_min = 5.0;
        spec.scale_max = 1.0;
        assert!(plan_scatter(&spec).is_err());
    }
}

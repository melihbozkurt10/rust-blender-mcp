//! Shader graph planning.
//!
//! A PBR material is a fixed shape: texture nodes feeding a Principled BSDF,
//! with a normal map node in the normal path, a displacement node if there is a
//! height map, and a shared mapping node if the UVs need scaling. Working that
//! shape out here means the graph is identical every time, is laid out neatly,
//! and can be checked without Blender.

use blender_protocol::{
    BlenderError, Result,
    geometry_nodes::{PlannedLink, PlannedNode, PlannedSocket},
    math::{Color4, Vec2, Vec3, check_non_negative, check_range},
    node_graph::{PropertyAssignment, PropertyValue, SocketDefault, SocketSelector},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which map a texture is, which decides where it is wired and whether it is
/// colour or data.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MapKind {
    BaseColor,
    Roughness,
    Metallic,
    Normal,
    Height,
    AmbientOcclusion,
    Emission,
    Alpha,
    Specular,
}

impl MapKind {
    /// Whether the image holds data rather than colour. Getting this wrong --
    /// treating a roughness map as sRGB -- is the most common texturing
    /// mistake there is, so it is decided here rather than left to the caller.
    pub const fn is_data(self) -> bool {
        !matches!(self, MapKind::BaseColor | MapKind::Emission)
    }

    /// The colour space to load the image in.
    pub const fn colorspace(self) -> &'static str {
        if self.is_data() { "Non-Color" } else { "sRGB" }
    }

    /// Which Principled input this map drives, if it drives one directly.
    pub const fn principled_socket(self) -> Option<&'static str> {
        match self {
            MapKind::BaseColor => Some("Base Color"),
            MapKind::Roughness => Some("Roughness"),
            MapKind::Metallic => Some("Metallic"),
            MapKind::Emission => Some("Emission Color"),
            MapKind::Alpha => Some("Alpha"),
            MapKind::Specular => Some("Specular IOR Level"),
            // Normal goes through a normal map node, height through
            // displacement, AO through a mix with base colour.
            MapKind::Normal | MapKind::Height | MapKind::AmbientOcclusion => None,
        }
    }

    /// Which output of the image node carries the value.
    pub const fn source_socket(self) -> &'static str {
        match self {
            MapKind::Alpha => "Alpha",
            _ => "Color",
        }
    }
}

/// One texture to wire in.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TextureMap {
    pub kind: MapKind,
    /// Image data-block name or id, already loaded.
    pub image: String,
}

/// A PBR material to build.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PbrSpec {
    /// Textures to wire in. An empty list produces a plain Principled setup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maps: Vec<TextureMap>,
    /// Base colour, used when there is no base colour map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color: Option<Color4>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f64>,
    /// Tiling applied to every texture through one shared mapping node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_scale: Option<Vec2>,
    /// Strength of the normal map, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_strength: Option<f64>,
    /// Displacement scale, if there is a height map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displacement_scale: Option<f64>,
}

/// A graph ready to be sent to Blender.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphPlan {
    pub nodes: Vec<PlannedNode>,
    pub links: Vec<PlannedLink>,
}

impl GraphPlan {
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn has_node(&self, key: &str) -> bool {
        self.nodes.iter().any(|n| n.key == key)
    }

    pub fn link_between(&self, from: &str, to: &str) -> bool {
        self.links
            .iter()
            .any(|l| l.from.node == from && l.to.node == to)
    }
}

/// Column positions, so the generated graph is readable in the node editor.
const COLUMN_COORDINATES: f64 = -1400.0;
const COLUMN_MAPPING: f64 = -1200.0;
const COLUMN_TEXTURES: f64 = -900.0;
const COLUMN_ADJUST: f64 = -500.0;
const COLUMN_BSDF: f64 = -150.0;
const COLUMN_OUTPUT: f64 = 200.0;
const ROW_SPACING: f64 = 300.0;

impl PbrSpec {
    /// Build the graph.
    pub fn plan(&self) -> Result<GraphPlan> {
        if let Some(value) = self.roughness {
            check_range(value, 0.0, 1.0, "roughness")?;
        }
        if let Some(value) = self.metallic {
            check_range(value, 0.0, 1.0, "metallic")?;
        }
        if let Some(value) = self.normal_strength {
            check_non_negative(value, "normal_strength")?;
        }
        if let Some(value) = self.displacement_scale {
            check_non_negative(value, "displacement_scale")?;
        }

        let mut seen = std::collections::BTreeSet::new();
        for map in &self.maps {
            if !seen.insert(map.kind) {
                return Err(BlenderError::invalid_argument(format!(
                    "Two {:?} maps were given; a material has one of each.",
                    map.kind
                ))
                .with_detail_json("kind", &map.kind));
            }
        }

        let mut nodes = Vec::new();
        let mut links = Vec::new();

        nodes.push(node("bsdf", "ShaderNodeBsdfPrincipled", COLUMN_BSDF, 0.0));
        nodes.push(node(
            "output",
            "ShaderNodeOutputMaterial",
            COLUMN_OUTPUT,
            0.0,
        ));
        links.push(link("bsdf", "BSDF", "output", "Surface"));

        // Scalar defaults, only where no map overrides them.
        let mapped: std::collections::BTreeSet<MapKind> =
            self.maps.iter().map(|m| m.kind).collect();
        let mut defaults = Vec::new();
        if let Some(color) = self.base_color
            && !mapped.contains(&MapKind::BaseColor)
        {
            defaults.push(default_socket("Base Color", PropertyValue::Color(color)));
        }
        if let Some(roughness) = self.roughness
            && !mapped.contains(&MapKind::Roughness)
        {
            defaults.push(default_socket("Roughness", PropertyValue::Float(roughness)));
        }
        if let Some(metallic) = self.metallic
            && !mapped.contains(&MapKind::Metallic)
        {
            defaults.push(default_socket("Metallic", PropertyValue::Float(metallic)));
        }
        if !defaults.is_empty() {
            nodes[0].inputs = defaults;
        }

        // One coordinate and mapping pair feeds every texture, so tiling is
        // changed in one place rather than on each node.
        let needs_mapping = self.uv_scale.is_some() && !self.maps.is_empty();
        if needs_mapping {
            nodes.push(node(
                "coords",
                "ShaderNodeTexCoord",
                COLUMN_COORDINATES,
                0.0,
            ));
            let scale = self.uv_scale.unwrap();
            let mut mapping = node("mapping", "ShaderNodeMapping", COLUMN_MAPPING, 0.0);
            mapping.inputs = vec![SocketDefault {
                socket: SocketSelector::Name("Scale".into()),
                value: PropertyValue::Vec3(Vec3::new(scale.x, scale.y, 1.0)),
            }];
            nodes.push(mapping);
            links.push(link("coords", "UV", "mapping", "Vector"));
        }

        for (index, map) in self.maps.iter().enumerate() {
            let key = format!("tex_{}", map_key(map.kind));
            let row = -(index as f64) * ROW_SPACING;
            let mut texture = node(&key, "ShaderNodeTexImage", COLUMN_TEXTURES, row);
            texture.properties = vec![PropertyAssignment {
                name: "image".into(),
                value: PropertyValue::Image(blender_protocol::ids::ImageRef::from(
                    map.image.as_str(),
                )),
            }];
            nodes.push(texture);

            if needs_mapping {
                links.push(link("mapping", "Vector", &key, "Vector"));
            }

            match map.kind {
                MapKind::Normal => {
                    let normal_key = "normal_map";
                    let mut normal = node(normal_key, "ShaderNodeNormalMap", COLUMN_ADJUST, row);
                    if let Some(strength) = self.normal_strength {
                        normal.inputs =
                            vec![default_socket("Strength", PropertyValue::Float(strength))];
                    }
                    nodes.push(normal);
                    links.push(link(&key, "Color", normal_key, "Color"));
                    links.push(link(normal_key, "Normal", "bsdf", "Normal"));
                }
                MapKind::Height => {
                    let displacement_key = "displacement";
                    let mut displacement = node(
                        displacement_key,
                        "ShaderNodeDisplacement",
                        COLUMN_ADJUST,
                        row,
                    );
                    if let Some(scale) = self.displacement_scale {
                        displacement.inputs =
                            vec![default_socket("Scale", PropertyValue::Float(scale))];
                    }
                    nodes.push(displacement);
                    links.push(link(&key, "Color", displacement_key, "Height"));
                    links.push(link(
                        displacement_key,
                        "Displacement",
                        "output",
                        "Displacement",
                    ));
                }
                MapKind::AmbientOcclusion => {
                    // AO multiplies the base colour. Without a base colour map
                    // there is nothing to multiply, so it is skipped rather
                    // than wired somewhere it does not belong.
                    if !mapped.contains(&MapKind::BaseColor) {
                        continue;
                    }
                    let mix_key = "ao_mix";
                    let mut mix = node(mix_key, "ShaderNodeMix", COLUMN_ADJUST, row);
                    mix.properties = vec![
                        PropertyAssignment {
                            name: "data_type".into(),
                            value: PropertyValue::Enum("RGBA".into()),
                        },
                        PropertyAssignment {
                            name: "blend_type".into(),
                            value: PropertyValue::Enum("MULTIPLY".into()),
                        },
                    ];
                    // Every socket on this node is addressed by identifier.
                    // `ShaderNodeMix` carries one set of sockets per data type
                    // and calls them all the same thing: two inputs named
                    // `Factor`, four named `A`, four outputs named `Result`.
                    // A name here resolves to whichever comes first, which is
                    // the float pair, and the graph then silently multiplies
                    // the wrong things.
                    mix.inputs = vec![default_socket_id("Factor_Float", PropertyValue::Float(1.0))];
                    nodes.push(mix);
                    // The base colour texture feeds A, the AO map feeds B, and
                    // the result replaces the direct base-colour link.
                    links.retain(|l| !(l.to.node == "bsdf" && socket_is(&l.to, "Base Color")));
                    links.push(link_to_id("tex_base_color", "Color", mix_key, "A_Color"));
                    links.push(link_to_id(&key, "Color", mix_key, "B_Color"));
                    links.push(link_from_id(mix_key, "Result_Color", "bsdf", "Base Color"));
                }
                other => {
                    if let Some(socket) = other.principled_socket() {
                        links.push(link(&key, other.source_socket(), "bsdf", socket));
                    }
                }
            }
        }

        Ok(GraphPlan { nodes, links })
    }

    /// Which images the plan needs loaded, and in which colour space.
    pub fn required_images(&self) -> Vec<(String, &'static str)> {
        self.maps
            .iter()
            .map(|map| (map.image.clone(), map.kind.colorspace()))
            .collect()
    }
}

fn map_key(kind: MapKind) -> &'static str {
    match kind {
        MapKind::BaseColor => "base_color",
        MapKind::Roughness => "roughness",
        MapKind::Metallic => "metallic",
        MapKind::Normal => "normal",
        MapKind::Height => "height",
        MapKind::AmbientOcclusion => "ao",
        MapKind::Emission => "emission",
        MapKind::Alpha => "alpha",
        MapKind::Specular => "specular",
    }
}

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

/// A link whose destination is addressed by Blender's socket identifier.
///
/// Identifiers rather than indices, because an index shifts whenever Blender
/// adds a data type to a multi-type node, while `A_Color` stays `A_Color`.
fn link_to_id(from: &str, from_socket: &str, to: &str, to_identifier: &str) -> PlannedLink {
    PlannedLink {
        from: PlannedSocket {
            node: from.to_string(),
            socket: SocketSelector::Name(from_socket.to_string()),
        },
        to: PlannedSocket {
            node: to.to_string(),
            socket: SocketSelector::Identifier(to_identifier.to_string()),
        },
    }
}

/// A link whose source is addressed by identifier.
fn link_from_id(from: &str, from_identifier: &str, to: &str, to_socket: &str) -> PlannedLink {
    PlannedLink {
        from: PlannedSocket {
            node: from.to_string(),
            socket: SocketSelector::Identifier(from_identifier.to_string()),
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

/// A socket default addressed by identifier, for nodes whose socket names
/// repeat.
fn default_socket_id(identifier: &str, value: PropertyValue) -> SocketDefault {
    SocketDefault {
        socket: SocketSelector::Identifier(identifier.to_string()),
        value,
    }
}

fn socket_is(socket: &PlannedSocket, name: &str) -> bool {
    matches!(&socket.socket, SocketSelector::Name(n) if n == name)
}

/// A glass material, which is a different shape from a PBR one: transmission
/// with a matched IOR, and roughness that means something different.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GlassSpec {
    #[serde(default = "default_glass_ior")]
    pub ior: f64,
    #[serde(default)]
    pub roughness: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color4>,
    /// Use a dedicated Glass BSDF rather than a Principled with transmission.
    /// The Principled version renders better in EEVEE; the Glass BSDF is more
    /// physically direct in Cycles.
    #[serde(default)]
    pub use_glass_bsdf: bool,
}

fn default_glass_ior() -> f64 {
    1.45
}

impl GlassSpec {
    pub fn plan(&self) -> Result<GraphPlan> {
        check_range(self.ior, 1.0, 3.0, "ior")?;
        check_range(self.roughness, 0.0, 1.0, "roughness")?;

        let mut bsdf = if self.use_glass_bsdf {
            node("bsdf", "ShaderNodeBsdfGlass", COLUMN_BSDF, 0.0)
        } else {
            node("bsdf", "ShaderNodeBsdfPrincipled", COLUMN_BSDF, 0.0)
        };

        let mut inputs = vec![
            default_socket("Roughness", PropertyValue::Float(self.roughness)),
            default_socket("IOR", PropertyValue::Float(self.ior)),
        ];
        if !self.use_glass_bsdf {
            inputs.push(default_socket(
                "Transmission Weight",
                PropertyValue::Float(1.0),
            ));
        }
        if let Some(color) = self.color {
            inputs.push(default_socket(
                if self.use_glass_bsdf {
                    "Color"
                } else {
                    "Base Color"
                },
                PropertyValue::Color(color),
            ));
        }
        bsdf.inputs = inputs;

        let output = node("output", "ShaderNodeOutputMaterial", COLUMN_OUTPUT, 0.0);
        // Both Principled and Glass call their shader output `BSDF`.
        let surface = "BSDF";
        Ok(GraphPlan {
            nodes: vec![bsdf, output],
            links: vec![link("bsdf", surface, "output", "Surface")],
        })
    }
}

/// An emissive material.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmissiveSpec {
    pub color: Color4,
    #[serde(default = "default_emission_strength")]
    pub strength: f64,
    /// Emit only, with no diffuse component at all.
    #[serde(default)]
    pub pure: bool,
}

fn default_emission_strength() -> f64 {
    5.0
}

impl EmissiveSpec {
    pub fn plan(&self) -> Result<GraphPlan> {
        check_non_negative(self.strength, "strength")?;
        self.color.check_finite_named("color")?;

        let mut bsdf = if self.pure {
            node("bsdf", "ShaderNodeEmission", COLUMN_BSDF, 0.0)
        } else {
            node("bsdf", "ShaderNodeBsdfPrincipled", COLUMN_BSDF, 0.0)
        };
        bsdf.inputs = if self.pure {
            vec![
                default_socket("Color", PropertyValue::Color(self.color)),
                default_socket("Strength", PropertyValue::Float(self.strength)),
            ]
        } else {
            vec![
                default_socket("Emission Color", PropertyValue::Color(self.color)),
                default_socket("Emission Strength", PropertyValue::Float(self.strength)),
            ]
        };

        let output = node("output", "ShaderNodeOutputMaterial", COLUMN_OUTPUT, 0.0);
        let from_socket = if self.pure { "Emission" } else { "BSDF" };
        Ok(GraphPlan {
            nodes: vec![bsdf, output],
            links: vec![link("bsdf", from_socket, "output", "Surface")],
        })
    }
}

trait CheckFiniteNamed {
    fn check_finite_named(&self, field: &str) -> Result<()>;
}

impl CheckFiniteNamed for Color4 {
    fn check_finite_named(&self, field: &str) -> Result<()> {
        blender_protocol::math::Finite::check_finite(self, field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(kind: MapKind) -> TextureMap {
        TextureMap {
            kind,
            image: format!("{:?}.png", kind),
        }
    }

    fn spec(maps: Vec<TextureMap>) -> PbrSpec {
        PbrSpec {
            maps,
            base_color: None,
            roughness: None,
            metallic: None,
            uv_scale: None,
            normal_strength: None,
            displacement_scale: None,
        }
    }

    #[test]
    fn the_ao_mix_addresses_every_ambiguous_socket_by_identifier() {
        // `ShaderNodeMix` names two inputs `Factor`, four `A`, four `B` and
        // four outputs `Result`. Addressing any of them by name resolves to
        // the float variant, and the material then multiplies the wrong
        // things -- silently, and only visibly in a render.
        let spec = PbrSpec {
            maps: vec![
                TextureMap {
                    kind: MapKind::BaseColor,
                    image: "diff".into(),
                },
                TextureMap {
                    kind: MapKind::AmbientOcclusion,
                    image: "ao".into(),
                },
            ],
            base_color: None,
            roughness: None,
            metallic: None,
            uv_scale: None,
            normal_strength: None,
            displacement_scale: None,
        };
        let plan = spec.plan().unwrap();

        let mix = plan
            .nodes
            .iter()
            .find(|node| node.key == "ao_mix")
            .expect("a mix node");
        assert!(
            mix.inputs.iter().all(|input| matches!(
                &input.socket,
                SocketSelector::Identifier(id) if id == "Factor_Float"
            )),
            "the mix factor must be addressed by identifier: {:?}",
            mix.inputs
        );

        let touching_mix: Vec<&PlannedLink> = plan
            .links
            .iter()
            .filter(|link| link.from.node == "ao_mix" || link.to.node == "ao_mix")
            .collect();
        assert_eq!(touching_mix.len(), 3, "{touching_mix:?}");

        for link in touching_mix {
            let end = if link.to.node == "ao_mix" {
                &link.to
            } else {
                &link.from
            };
            match &end.socket {
                SocketSelector::Identifier(id) => assert!(
                    ["A_Color", "B_Color", "Result_Color"].contains(&id.as_str()),
                    "unexpected identifier `{id}`"
                ),
                other => panic!("the mix socket is addressed by {other:?}, not by identifier"),
            }
        }
    }

    #[test]
    fn a_bare_spec_is_just_a_principled_and_an_output() {
        let plan = spec(vec![]).plan().unwrap();
        assert_eq!(plan.node_count(), 2);
        assert!(plan.link_between("bsdf", "output"));
    }

    #[test]
    fn each_map_gets_a_texture_node_wired_to_the_right_input() {
        let plan = spec(vec![map(MapKind::BaseColor), map(MapKind::Roughness)])
            .plan()
            .unwrap();
        assert!(plan.has_node("tex_base_color"));
        assert!(plan.has_node("tex_roughness"));
        assert!(plan.link_between("tex_base_color", "bsdf"));
        assert!(plan.link_between("tex_roughness", "bsdf"));
    }

    #[test]
    fn a_normal_map_goes_through_a_normal_map_node() {
        let plan = spec(vec![map(MapKind::Normal)]).plan().unwrap();
        assert!(plan.has_node("normal_map"));
        assert!(plan.link_between("tex_normal", "normal_map"));
        assert!(plan.link_between("normal_map", "bsdf"));
        assert!(
            !plan.link_between("tex_normal", "bsdf"),
            "a normal map must never be wired straight into the BSDF"
        );
    }

    #[test]
    fn a_height_map_drives_displacement_on_the_output() {
        let plan = spec(vec![map(MapKind::Height)]).plan().unwrap();
        assert!(plan.has_node("displacement"));
        assert!(plan.link_between("displacement", "output"));
    }

    #[test]
    fn ambient_occlusion_multiplies_the_base_colour() {
        let plan = spec(vec![
            map(MapKind::BaseColor),
            map(MapKind::AmbientOcclusion),
        ])
        .plan()
        .unwrap();
        assert!(plan.has_node("ao_mix"));
        assert!(plan.link_between("tex_base_color", "ao_mix"));
        assert!(plan.link_between("tex_ao", "ao_mix"));
        assert!(plan.link_between("ao_mix", "bsdf"));
        // The direct base-colour link must have been replaced, not duplicated.
        assert!(
            !plan.link_between("tex_base_color", "bsdf"),
            "base colour should now reach the BSDF through the AO mix"
        );
    }

    #[test]
    fn ambient_occlusion_alone_is_skipped_rather_than_misplaced() {
        let plan = spec(vec![map(MapKind::AmbientOcclusion)]).plan().unwrap();
        assert!(
            !plan.has_node("ao_mix"),
            "there is no base colour to multiply"
        );
    }

    #[test]
    fn uv_scaling_adds_one_shared_mapping_chain() {
        let mut params = spec(vec![map(MapKind::BaseColor), map(MapKind::Roughness)]);
        params.uv_scale = Some(Vec2::new(4.0, 4.0));
        let plan = params.plan().unwrap();
        assert!(plan.has_node("coords") && plan.has_node("mapping"));
        assert!(plan.link_between("mapping", "tex_base_color"));
        assert!(plan.link_between("mapping", "tex_roughness"));
        assert_eq!(
            plan.nodes
                .iter()
                .filter(|n| n.node_type == "ShaderNodeMapping")
                .count(),
            1,
            "one mapping node, shared"
        );
    }

    #[test]
    fn scalar_values_are_not_set_where_a_map_overrides_them() {
        let mut params = spec(vec![map(MapKind::Roughness)]);
        params.roughness = Some(0.2);
        params.metallic = Some(1.0);
        let plan = params.plan().unwrap();
        let bsdf = plan.nodes.iter().find(|n| n.key == "bsdf").unwrap();
        let names: Vec<String> = bsdf
            .inputs
            .iter()
            .map(|i| match &i.socket {
                SocketSelector::Name(n) => n.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert!(names.contains(&"Metallic".to_string()));
        assert!(
            !names.contains(&"Roughness".to_string()),
            "the roughness map drives it, so a default would be dead weight"
        );
    }

    #[test]
    fn duplicate_maps_are_refused() {
        let error = spec(vec![map(MapKind::Normal), map(MapKind::Normal)])
            .plan()
            .unwrap_err();
        assert!(error.message.contains("one of each"));
    }

    #[test]
    fn data_maps_are_flagged_non_colour() {
        assert_eq!(MapKind::Roughness.colorspace(), "Non-Color");
        assert_eq!(MapKind::Normal.colorspace(), "Non-Color");
        assert_eq!(MapKind::BaseColor.colorspace(), "sRGB");
        assert_eq!(MapKind::Emission.colorspace(), "sRGB");
    }

    #[test]
    fn required_images_report_their_colour_space() {
        let params = spec(vec![map(MapKind::BaseColor), map(MapKind::Normal)]);
        let images = params.required_images();
        assert_eq!(images.len(), 2);
        assert!(images.iter().any(|(_, space)| *space == "Non-Color"));
    }

    #[test]
    fn glass_sets_transmission_on_a_principled_by_default() {
        let plan = GlassSpec {
            ior: 1.45,
            roughness: 0.0,
            color: None,
            use_glass_bsdf: false,
        }
        .plan()
        .unwrap();
        let bsdf = plan.nodes.iter().find(|n| n.key == "bsdf").unwrap();
        assert_eq!(bsdf.node_type, "ShaderNodeBsdfPrincipled");
        assert!(bsdf.inputs.iter().any(|i| matches!(
            &i.socket, SocketSelector::Name(n) if n == "Transmission Weight"
        )));
    }

    #[test]
    fn glass_can_use_the_dedicated_bsdf() {
        let plan = GlassSpec {
            ior: 1.5,
            roughness: 0.1,
            color: None,
            use_glass_bsdf: true,
        }
        .plan()
        .unwrap();
        assert_eq!(plan.nodes[0].node_type, "ShaderNodeBsdfGlass");
    }

    #[test]
    fn an_impossible_ior_is_refused() {
        assert!(
            GlassSpec {
                ior: 0.5,
                roughness: 0.0,
                color: None,
                use_glass_bsdf: false
            }
            .plan()
            .is_err()
        );
    }

    #[test]
    fn pure_emission_uses_an_emission_node() {
        let plan = EmissiveSpec {
            color: Color4::WHITE,
            strength: 10.0,
            pure: true,
        }
        .plan()
        .unwrap();
        assert_eq!(plan.nodes[0].node_type, "ShaderNodeEmission");
        assert!(plan.link_between("bsdf", "output"));
    }

    #[test]
    fn non_pure_emission_stays_on_the_principled() {
        let plan = EmissiveSpec {
            color: Color4::WHITE,
            strength: 10.0,
            pure: false,
        }
        .plan()
        .unwrap();
        assert_eq!(plan.nodes[0].node_type, "ShaderNodeBsdfPrincipled");
    }
}

# Shader and geometry node graphs

Node graphs are where a naive MCP server falls apart. A Principled setup with
base colour, roughness, normal and displacement maps is a dozen nodes, twenty
sockets and fifteen links, and every one of them is a chance to mistype a socket
name. Doing that as fifteen round trips is slow; doing it as a string of Python
is the thing this project exists not to do.

So graphs are **planned in Rust and applied in one call**.

## The plan

```rust
pub struct GraphPlan {
    pub nodes: Vec<PlannedNode>,
    pub links: Vec<PlannedLink>,
}

pub struct PlannedNode {
    pub key: String,          // referred to by links; not Blender's name
    pub node_type: String,    // bl_idname, checked against capabilities
    pub location: (f64, f64),
    pub inputs: Vec<PlannedSocket>,
    pub properties: Map<String, PropertyValue>,
}

pub struct PlannedLink {
    pub from: PlannedEndpoint,   // {node: key, socket: name-or-index}
    pub to:   PlannedEndpoint,
}
```

`key` is a plan-local handle. Blender assigns its own names; the plan never has
to guess what they will be, and a link refers to `"noise"` rather than to
`"Noise Texture.001"`.

The plan is validated in Rust before it is sent: every link endpoint must name a
node in the plan, every node type must exist in the connected build, and socket
values must be the right kind. Then one bridge call —
`shader.graph.build` or `geometry_nodes.graph.build` — creates everything and
links it up.

## Built-in plans

`blender-domain` builds plans for the setups that come up constantly:

| Spec | Produces |
| --- | --- |
| `PbrSpec` | Principled BSDF with any subset of base colour, roughness, metallic, normal, height, AO, emission, alpha and specular maps; a shared texture-coordinate and mapping pair for tiling; a Normal Map node; a Displacement node |
| `GlassSpec` | Glass BSDF, or Principled with transmission |
| `EmissiveSpec` | Emission shader, or Principled with emission, or a pure emit |
| `plan_scatter` | Distribute Points on Faces → Instance on Points → Realize, with density, seed, scale and rotation randomisation |
| `plan_array_along_curve` | Curve to Points → Instance on Points, with alignment |

Reach them through `workflow.material.pbr`, `workflow.material.glass`,
`workflow.material.emissive`, `geometry_nodes.scatter` and
`geometry_nodes.array_along_curve`.

```jsonc
{
  "name": "workflow.material.pbr",
  "arguments": {
    "name": "Concrete",
    "maps": [
      {"kind": "base_color", "image": "concrete_diff_2k"},
      {"kind": "roughness",  "image": "concrete_rough_2k"},
      {"kind": "normal",     "image": "concrete_nor_gl_2k"}
    ],
    "uv_scale": {"x": 2.0, "y": 2.0},
    "assign_to": ["Floor"]
  }
}
```

One call. The graph is laid out in readable columns — coordinates, mapping,
textures, adjustment, BSDF, output — so it is workable by hand afterwards.

### Colour spaces are decided, not asked

`MapKind::is_data()` decides: base colour and emission are sRGB, everything else
is `Non-Color`. Loading a roughness map as sRGB is the most common texturing
mistake there is, and it produces a material that is subtly wrong in a way that
is hard to see and easy to ship. The caller does not get to make that mistake
here.

## Working node by node

When a plan does not fit, the individual tools are there:

```
shader.node.create        shader.node.update        shader.node.delete
shader.node.get           shader.node.list
shader.link.create        shader.link.delete        shader.link.list
shader.socket.get         shader.socket.set_default
shader.tree.get           shader.tree.clear
```

with the same shape for `geometry_nodes.*`.

Node types are checked against the running build's registered `bl_idname`s, which
the add-on collects by walking `bpy.types` — `ShaderNode.__subclasses__()` returns
nothing useful, because Blender creates RNA classes lazily.

## Sockets: name, identifier, or index

Blender node sockets have both a display name and an identifier, and they are
frequently different. Worse, a node can have several sockets with the *same*
display name — `FunctionNodeRandomValue` has four outputs all called `Value`,
one per data type.

So socket lookup:

1. tries the display name;
2. falls back to the identifier;
3. accepts an explicit index when you need to be unambiguous.

An unknown socket comes back with the ones that exist:

```jsonc
{
  "code": "INVALID_NODE_SOCKET",
  "message": "`Fac` is not an input on this node.",
  "details": {
    "node": "noise",
    "requested": "Fac",
    "available": ["Vector", "W", "Scale", "Detail", "Roughness", "Lacunarity", "Distortion"]
  }
}
```

The four-`Value` case is not hypothetical — it was found by running against real
Blender, and it is why `link_from_index` exists in the plan builder and has a
regression test.

## Geometry node groups

A geometry node group has an **interface**: the inputs and outputs the modifier
exposes. Those are managed separately from the nodes:

```
geometry_nodes.interface.add_socket      geometry_nodes.interface.update_socket
geometry_nodes.interface.delete_socket   geometry_nodes.interface.list
```

and the group is attached to an object with `geometry_nodes.modifier.attach`.
Blender 4.0 replaced `node_group.inputs` with `node_group.interface`; the add-on
handles both, so a group built here works on 4.2 and on 5.x.

## Invalidation

Editing a graph emits a `node_tree_invalidated` event rather than a description
of what changed. Serialising a whole node tree on every tweak would cost more
than it saves; the server marks the tree stale, and `shader.tree.get` re-reads it
when someone actually asks.

## What this is not

There is no tool that takes a node-graph description in a made-up text format and
parses it. The plan is a typed structure, validated in Rust, transported as JSON.
The only thing crossing the wire is data.

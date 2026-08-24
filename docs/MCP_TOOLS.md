# The tools

247 tools across 18 categories. Every one has a JSON schema with declared
properties — there is no free-form payload anywhere — and every one is `READ`,
`WRITE` or `EXTERNAL_SIDE_EFFECT`.

To see this list from your own build, including schema sizes:

```bash
cargo run -p blender-mcp-server --example tool_inventory -- --names
```

Only the `core` category is registered at startup. See
[TOOL_CATEGORIES.md](TOOL_CATEGORIES.md) for how the rest arrive.

## Naming

`domain.thing.verb`, consistently:

- `object.create`, `object.delete`, `object.list`, `object.get`
- `shader.node.create`, `shader.link.create`, `shader.socket.set_default`
- `rig.bone.parent`, `rig.constraint.add`

A tool that forwards to the bridge has the same name as the bridge operation, so
the name in a log, an error and a batch step all mean the same thing. Tools that
do their work in the server — planning, batching, asset handling — follow the
same scheme.

Preferring coherent operations over hundreds of setters is deliberate:
`camera.update` takes lens, sensor, clipping and projection in one typed call
rather than offering `camera.set_lens`, `camera.set_sensor_width` and so on.

---

## core — 10 tools

Registered at startup. Enough to look at a scene and decide what else to load.

| Tool | Kind |
| --- | --- |
| `scene.summary` | READ |
| `object.list`, `object.get` | READ |
| `selection.get` | READ |
| `blender.status`, `blender.capabilities` | READ |
| `tools.categories.list`, `tools.categories.enable`, `tools.categories.disable` | READ |
| `batch.execute` | WRITE |

`scene.summary` is the intended first call: object counts by type, material and
collection totals, selection, active camera, frame range, render engine.

## scene — 40 tools

Scene settings, objects, collections and selection.

**Scene** `scene.get`, `scene.settings.update`, `scene.statistics`,
`scene.world.get`, `scene.world.update`, `scene.snapshot`, `scene.diff`

**Objects** `object.create`, `object.delete`, `object.duplicate`,
`object.rename`, `object.transform`, `object.transform.apply`,
`object.set_parent`, `object.clear_parent`, `object.origin.set`, `object.join`,
`object.separate`, `object.convert`, `object.hide`, `object.show`,
`object.set_display`

**Collections** `collection.create`, `collection.delete`, `collection.get`,
`collection.list`, `collection.rename`, `collection.link_object`,
`collection.unlink_object`, `collection.move_object`,
`collection.set_visibility`

**Selection** `selection.set`, `selection.add`, `selection.remove`,
`selection.clear`, `selection.set_active`

**Surfaces** `scene.surface.inspect`, `scene.surface.raycast`,
`scene.openings.inspect`, `scene.openings.mark`

`scene.snapshot` and `scene.diff` are answered from the server's cache, not from
Blender: take a snapshot, do some work, ask what changed. A revision that has
aged out of the history returns `REVISION_EXPIRED` rather than a partial answer.

`scene.surface.inspect` groups an object's faces into planar regions and
classifies each as a wall, a floor, a ceiling or other, in world space with the
object's own rotation already applied. A thousand triangles of one wall come
back as one wall, with the centre, normal, in-plane tangent and extent a
placement needs. `scene.surface.raycast` casts one ray at an explicit list of
objects -- never at the whole scene, so a stray helper cannot quietly become the
answer.

Openings are doors and windows somebody marked with `scene.openings.mark`.
Nothing looks for holes in geometry: a gap in a mesh is not a doorway, and where
nobody has marked one the answer says so rather than guessing.

## materials — 9 tools

`material.create`, `material.delete`, `material.duplicate`, `material.get`,
`material.list`, `material.update`, `material.assign`, `material.unassign`,
`material.slot.list`

For building a material rather than tweaking one, use
`workflow.material.pbr`/`glass`/`emissive`, which plan the whole graph in Rust.

## shader_nodes — 12 tools

`shader.node.create`, `shader.node.delete`, `shader.node.get`,
`shader.node.list`, `shader.node.update`, `shader.link.create`,
`shader.link.delete`, `shader.link.list`, `shader.socket.get`,
`shader.socket.set_default`, `shader.tree.get`, `shader.tree.clear`

Node types are validated against the connected build's registered
`bl_idname`s, and an unknown socket name comes back with the sockets that exist.
See [NODE_GRAPHS.md](NODE_GRAPHS.md).

## lights — 6 tools

`light.create`, `light.delete`, `light.get`, `light.list`, `light.update`,
`light.look_at`

`light.look_at` orients a light at a point; the rotation is computed in Rust.

## modifiers — 8 tools

`modifier.add`, `modifier.remove`, `modifier.update`, `modifier.get`,
`modifier.list`, `modifier.move`, `modifier.copy`, `modifier.apply`

Modifier types are capability-checked. Modifiers cannot hold custom properties in
Blender, so they are identified by Blender's own `persistent_uid`.

## mesh — 20 tools

**Create and inspect** `mesh.create`, `mesh.info`, `mesh.analyze`,
`mesh.vertices.get`, `mesh.faces.get`

**Edit** `mesh.extrude`, `mesh.inset`, `mesh.bevel`, `mesh.subdivide`,
`mesh.loop_cut`, `mesh.bridge_edge_loops`, `mesh.fill`, `mesh.dissolve`,
`mesh.delete_elements`, `mesh.merge_vertices`, `mesh.remove_doubles`

**Repair** `mesh.triangulate`, `mesh.quads_from_tris`,
`mesh.normals.recalculate`, `mesh.normals.flip`

Editing goes through `bmesh.ops` rather than `bpy.ops.mesh.*`, so it works
headless and does not depend on which area happens to be under the cursor.
Element indices are validated against the mesh's current revision; editing a mesh
that has changed underneath you gives `TOPOLOGY_STALE` rather than silently
operating on the wrong vertices.

## animation — 26 tools

**Keyframes** `animation.keyframe.insert`, `animation.keyframe.delete`,
`animation.keyframe.list`, `animation.interpolation.set`

**F-curves** `animation.fcurve.list`, `animation.fcurve.get`,
`animation.fcurve.update`

**Actions** `animation.action.create`, `animation.action.assign`,
`animation.action.delete`, `animation.action.get`, `animation.action.list`

**NLA** `animation.nla.track.create`, `animation.nla.track.delete`,
`animation.nla.track.list`, `animation.nla.strip.create`,
`animation.nla.strip.update`, `animation.nla.strip.delete`

**Frames and helpers** `animation.frame.get`, `animation.frame.set`,
`animation.range.get`, `animation.range.set`, `animation.loop`,
`animation.create_move`, `animation.create_rotation`, `animation.create_scale`

Blender 4.4 replaced `Action.fcurves` with slotted layered Actions. One
compatibility module absorbs the difference; the tools are the same either way.

## geometry_nodes — 21 tools

**Groups** `geometry_nodes.group.create`, `.delete`, `.get`, `.list`

**Nodes and links** `geometry_nodes.node.create`, `.delete`, `.get`, `.list`,
`.update`, `geometry_nodes.link.create`, `.delete`, `.list`

**Interface** `geometry_nodes.interface.add_socket`, `.update_socket`,
`.delete_socket`, `.list`

**Modifiers** `geometry_nodes.modifier.attach`, `.detach`, `.list`

**Whole graphs** `geometry_nodes.graph.build`, `geometry_nodes.tree.get`

`geometry_nodes.graph.build` applies a complete plan in one call, which is how
the scatter and array workflows work.

## camera — 11 tools

`camera.create`, `camera.delete`, `camera.get`, `camera.list`, `camera.update`,
`camera.set_active`, `camera.look_at`, `camera.track_object`,
`camera.clear_tracking`, `camera.depth_of_field.update`, `camera.auto_frame`

`camera.auto_frame` solves for the distance that fits a target's bounding box in
frame, taking sensor fit and aspect ratio into account. The solve is closed-form,
in Rust, and unit-tested — no trial renders.

## render — 6 tools

`render.execute` (EXTERNAL), `render.viewport_screenshot` (EXTERNAL),
`render.settings.get`, `render.settings.update`, `render.engine.set`,
`render.artifacts.list`

Output is written to the managed renders root and returned as an artifact
reference — an id, a relative path, a size, a MIME type — never as inline base64.
`render.viewport_screenshot` refuses in background mode with a message that says
why.

## import_export — 5 tools

`io.import` (EXTERNAL), `io.export` (EXTERNAL), `io.capabilities`,
`file.save` (EXTERNAL), `file.info`

Formats are capability-checked against the running build. Paths are managed-root
relative; see [SECURITY.md](SECURITY.md#filesystem).

## uv_texture — 22 tools

**Unwrapping** `uv.unwrap.angle_based`, `uv.unwrap.conformal`,
`uv.smart_project`, `uv.cube_project`, `uv.cylinder_project`,
`uv.sphere_project`, `uv.project_from_view`

**Seams and islands** `uv.mark_seam`, `uv.clear_seam`, `uv.pack_islands`,
`uv.average_island_scale`

**Maps** `uv.map.create`, `uv.map.delete`, `uv.map.set_active`, `uv.maps.list`

**Images** `image.load`, `image.list`, `image.get`, `image.reload`,
`image.remove`

**Baking** `texture.bake` (EXTERNAL)

`image.load` takes a colour space; set `Non-Color` for normal, roughness and
metallic maps. Loading a data map as sRGB is the most common texturing mistake
there is, which is why the asset import path decides it for you.

## rigging — 21 tools

**Armatures and bones** `rig.armature.create`, `.get`, `.list`,
`rig.bone.create`, `.delete`, `.get`, `.list`, `.update`, `.parent`, `.mirror`

**Constraints** `rig.constraint.add`, `.update`, `.remove`, `.list`

**Skinning** `rig.parent_mesh`, `rig.auto_weights`,
`rig.vertex_group.create`, `.delete`, `.assign`, `.list`, `.normalize`

## rig_diagnostics — 7 tools

`rig.diagnostics.health`, `rig.diagnostics.naming`, `rig.diagnostics.symmetry`,
`rig.diagnostics.weights`, `rig.fix.naming`, `rig.fix.mirror_bones`,
`rig.fix.normalize_weights`

Separate from `rigging` so a client that only wants to *check* a rig does not
have to load the tools that can modify one.

## assets — 5 tools

`asset.providers`, `asset.search`, `asset.get`, `asset.download` (EXTERNAL),
`asset.import` (EXTERNAL)

See [ASSETS.md](ASSETS.md).

## utilities — 7 tools

`scene.cleanup`, `scene.purge_orphans`, `scene.batch_rename`,
`scene.apply_transforms`, `scene.find_duplicates`,
`scene.find_missing_textures`, `scene.mesh_analysis`

Scene hygiene: the things a person does before handing a file to someone else.

## workflows — 11 tools

Multi-step operations planned in Rust, executed as a sequence, and rolled back
with compensating actions when a step fails.

| Tool | What it does |
| --- | --- |
| `workflow.material.pbr` | Create a material and build a full PBR graph |
| `workflow.material.glass` | Glass or transmissive Principled setup |
| `workflow.material.emissive` | Emission shader, optionally pure emit |
| `workflow.lighting.three_point` | Key/fill/rim rig sized to a target |
| `workflow.render.studio` | Studio setup: camera framing, lights, engine |
| `workflow.product_turntable` | Turntable animation around a subject |
| `workflow.model.create_wall` | One wall from a typed spec |
| `workflow.model.create_wall_run` | A run of connected walls with openings |
| `workflow.export.prepare` | Validate a scene against an export profile |
| `geometry_nodes.scatter` | Scatter geometry over a surface |
| `geometry_nodes.array_along_curve` | Array instances along a curve |

Each reports every step it took, and what it undid if something failed. See
[TRANSACTIONS.md](TRANSACTIONS.md).

## Conventions

**Pagination.** Every listing takes `limit` (default 100, max 1000) and
`cursor`, and returns `next_cursor` when there is more. A 50 000-object scene
cannot be dumped into a context window by accident.

**References.** Anywhere a tool takes an object, material, collection or image,
it accepts either an id or a name. Ids are stable across renames; names are
resolved at call time.

**Validation before the wire.** Ranges, enums, finiteness, frame ordering and
capability requirements are checked in Rust. The error names the field, gives the
offending value, and lists the valid ones.

**Artifacts, not blobs.** Anything large is a reference to a file in a managed
root.

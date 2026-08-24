# Demo: a product studio shot, start to finish

Fourteen tool calls take an empty Blender to a rendered PNG: a bevelled crate on
a backdrop, a wood material, three-point lighting, a camera framed on the
subject, and a 960×540 EEVEE render.

**Not one of them sends code.** Every argument below is a value in a schema the
server generated from a Rust struct.

This is a recording, not a design sketch. The calls, the replies and the timings
were captured by running the sequence against Blender 5.1.1 on the machine in
[`benchmarks/results/latest.md`](../benchmarks/results/latest.md). Replies are
abbreviated to the interesting fields; nothing has been edited into working
order.

## Setting up

The server needs these categories. Skip this if it runs with
`BLENDER_MCP_EAGER_TOOLS=1`.

```jsonc
{"name": "tools.categories.enable", "arguments": {"category": "scene"}}
{"name": "tools.categories.enable", "arguments": {"category": "mesh"}}
{"name": "tools.categories.enable", "arguments": {"category": "modifiers"}}
{"name": "tools.categories.enable", "arguments": {"category": "materials"}}
{"name": "tools.categories.enable", "arguments": {"category": "camera"}}
{"name": "tools.categories.enable", "arguments": {"category": "render"}}
{"name": "tools.categories.enable", "arguments": {"category": "workflows"}}
{"name": "tools.categories.enable", "arguments": {"category": "utilities"}}
```

---

## 1. Look before touching · `scene.summary` · 6.8 ms

The intended first call of any session. It is in the always-on `core` category,
so it costs nothing in context.

```jsonc
{"name": "scene.summary", "arguments": {}}
```
```jsonc
{"revision": 0, "scene": "Scene",
 "objects": {"total": 3, "mesh": 1, "light": 1, "camera": 1},
 "materials": 2, "collections": 1, "frame_start": 1, "frame_end": 250,
 "fps": 24.0, "render_engine": "BLENDER_EEVEE", "active_camera": "Camera",
 "selected": ["Cube"], "active_object": "Cube"}
```

## 2. Clear the factory scene · `object.list` + `object.delete` · 13.9 ms

```jsonc
{"name": "object.list", "arguments": {"limit": 100}}
```
```jsonc
{"objects": [{"id": "01a2f577-…", "name": "Camera", "type": "CAMERA", …},
             {"id": "13eb12d8-…", "name": "Cube",   "type": "MESH",   …},
             {"id": "70c54e18-…", "name": "Light",  "type": "LIGHT",  …}]}
```

Objects are addressed by a stable `id` that survives renaming. A name works too,
but only until somebody renames it.

```jsonc
{"name": "object.delete",
 "arguments": {"objects": ["01a2f577-…", "13eb12d8-…", "70c54e18-…"]}}
```
```jsonc
{"deleted": [{"id": "01a2f577-…", "name": "Camera"},
             {"id": "13eb12d8-…", "name": "Cube"},
             {"id": "70c54e18-…", "name": "Light"}],
 "revision": 1}
```

## 3. The subject · `object.create` · 27.1 ms

`dimensions` sets real-world size directly, so there is no scale arithmetic to
get wrong. The reply carries the resulting scale and the mesh statistics.

```jsonc
{"name": "object.create",
 "arguments": {"type": "CUBE", "name": "Crate",
               "dimensions": {"x": 0.6, "y": 0.4, "z": 0.35}}}
```
```jsonc
{"object": {"id": "770c89e1-…", "name": "Crate", "type": "MESH",
            "scale": {"x": 0.3, "y": 0.2, "z": 0.175},
            "dimensions": {"x": 0.6, "y": 0.4, "z": 0.35},
            "mesh": {"vertices": 8, "edges": 12, "faces": 6, "triangles": 12}}}
```

## 4. Sit it on the floor · `object.transform` · 2.1 ms

Half the crate's height, so it rests on `z = 0` rather than sinking through it.

```jsonc
{"name": "object.transform",
 "arguments": {"object": "770c89e1-…", "location": {"x": 0, "y": 0, "z": 0.175}}}
```
```jsonc
{"object": {"name": "Crate", "location": {"x": 0.0, "y": 0.0, "z": 0.175}, …}}
```

## 5. A backdrop · `object.create` · 6.7 ms

```jsonc
{"name": "object.create",
 "arguments": {"type": "PLANE", "name": "Backdrop",
               "dimensions": {"x": 6, "y": 6, "z": 0}}}
```

## 6. Break the edges · `modifier.add` · 6.9 ms

Sharp CG edges are the giveaway. A bevel is the fix.

Note the shape of a property value: `{"float": 0.008}`, not `0.008`. Values carry
their own type, which is exactly why there is no `setattr(modifier, name, value)`
anywhere in the add-on — a string cannot land in a float field.

```jsonc
{"name": "modifier.add",
 "arguments": {"object": "770c89e1-…", "type": "BEVEL", "name": "Edges",
               "properties": [{"name": "width",        "value": {"float": 0.008}},
                              {"name": "segments",     "value": {"int": 3}},
                              {"name": "limit_method", "value": {"string": "ANGLE"}}]}}
```
```jsonc
{"modifier": {"id": "1277244409", "name": "Edges", "type": "BEVEL", "index": 0,
              "properties": [{"name": "width",        "value": {"float": 0.008}},
                             {"name": "segments",     "value": {"int": 3}},
                             {"name": "limit_method", "value": {"string": "ANGLE"}},
                             {"name": "affect",       "value": {"string": "EDGES"}}, …]}}
```

The reply lists every property the modifier actually has, so the next call does
not have to guess a name.

## 7. A material, assigned in the same call · `material.create` · 5.3 ms

`assign_to` saves a round trip, and `principled` is a typed surface rather than a
node graph you have to wire by hand.

```jsonc
{"name": "material.create",
 "arguments": {"name": "CrateWood", "use_nodes": true,
               "principled": {"base_color": [0.32, 0.20, 0.11, 1.0],
                              "roughness": 0.62, "metallic": 0.0},
               "assign_to": ["770c89e1-…"]}}
```
```jsonc
{"material": {"id": "8e4b2ceb-…", "name": "CrateWood", "use_nodes": true,
              "users": 1, "node_count": 2,
              "principled": {"base_color": {"r": 0.32, "g": 0.2, "b": 0.11, "a": 1.0},
                             "roughness": 0.62, "metallic": 0.0, "ior": 1.5}}}
```

## 8. Three-point lighting, sized to the subject · `workflow.lighting.three_point` · 28.5 ms

One call. The key/fill/rim positions and energies are solved in Rust from the
subject's measured bounding box — no trial renders, no magic numbers. If any step
fails, the workflow rolls back what it already did.

```jsonc
{"name": "workflow.lighting.three_point",
 "arguments": {"target": "770c89e1-…", "key_energy": 400, "name_prefix": "Studio"}}
```
```jsonc
{"workflow": "workflow.lighting.three_point", "success": true,
 "steps": [{"name": "measure the subject", "op": "object.get", "status": "completed", …},
           {"name": "key light",  "op": "light.create", "status": "completed", …},
           {"name": "fill light", "op": "light.create", "status": "completed", …},
           {"name": "rim light",  "op": "light.create", "status": "completed", …}]}
```

## 9. A camera · `camera.create` · 2.9 ms

`lens` is `{"millimetres": 85.0}` — the alternative is `{"fov_degrees": …}`, and
the schema makes you say which you meant.

```jsonc
{"name": "camera.create",
 "arguments": {"name": "ShotCam", "location": {"x": 1.4, "y": -1.6, "z": 0.9},
               "lens": {"millimetres": 85.0}, "set_active": true}}
```
```jsonc
{"camera": {"id": "5fcc6bf6-…", "name": "ShotCam", "lens_mm": 85.0,
            "sensor_width": 36.0, "projection": "PERSP", "is_active": true,
            "location": {"x": 1.4, "y": -1.6, "z": 0.9}}}
```

## 10. Frame the shot · `camera.auto_frame` · 6.2 ms

The framing solve — where the camera has to sit, and how to aim it, so the
subject fills the frame with 20% padding at the lens it already has — happens in
Rust. Blender is told the answer, not asked to iterate towards it.

```jsonc
{"name": "camera.auto_frame",
 "arguments": {"camera": "5fcc6bf6-…", "objects": ["770c89e1-…"], "padding": 0.2}}
```
```jsonc
{"camera": {"name": "ShotCam", "lens_mm": 85.0,
            "location": {"x": 2.534, "y": -2.896, "z": 1.487},
            "rotation_euler": {"x": 1.242, "y": -0.0, "z": 0.719}}}
```

The camera moved from where it was put to where it needed to be, and the lens did
not change.

## 11. Check the scene · `scene.statistics` · 2.0 ms

```jsonc
{"name": "scene.statistics", "arguments": {}}
```
```jsonc
{"objects": {"total": 6, "mesh": 2, "light": 3, "camera": 1},
 "vertices": 12, "edges": 16, "faces": 7, "triangles": 14,
 "materials": 3, "modifiers": 1, "hidden_objects": 0, "revision": 58}
```

Two meshes, three lights, one camera, one modifier. That is the scene as
described, and the revision number says how many mutations it took.

## 12. Render · `render.execute` · 10.5 s

The only slow call in the demo, and every millisecond of it is Blender.

```jsonc
{"name": "render.execute",
 "arguments": {"camera": "5fcc6bf6-…", "scope": {"frame": 1},
               "name": "crate_studio", "engine": "EEVEE",
               "resolution_x": 960, "resolution_y": 540,
               "samples": 32, "format": "PNG"}}
```
```jsonc
{"artifacts": [{"artifact_id": "1d342cf8-…",
                "relative_path": "crate_studio.png", "root": "renders",
                "mime_type": "image/png", "size_bytes": 488243,
                "width": 960, "height": 540, "frame": 1,
                "engine": "BLENDER_EEVEE", "duration_ms": 10523}],
 "count": 1, "format": "PNG"}
```

The image comes back as an artifact reference — a path inside the managed
`renders` root — not as half a megabyte of base64 in the conversation.

`engine: "EEVEE"` is the name the schema accepts; the bridge maps it to whatever
this build actually ships (`BLENDER_EEVEE_NEXT` on 4.2+, `BLENDER_EEVEE` here),
which is why the reply names a different string than the request.

## 13. Tidy up, carefully · `scene.cleanup` · 6.0 ms

`dry_run` first. Always.

```jsonc
{"name": "scene.cleanup", "arguments": {"purge_orphans": true, "dry_run": true}}
```
```jsonc
{"dry_run": true,
 "passes": {"purge_orphans": {"would_remove": {"materials": ["Dots Stroke", "Material"]},
                              "count": 2}},
 "revision": 58}
```

Two orphaned materials left over from the factory file. Run it again without
`dry_run` to remove them.

---

## What this took

| | |
| --- | ---: |
| Tool calls | 14 |
| Calls that sent code | 0 |
| Total wall time | ~10.6 s |
| Of which was the render | 10.5 s |
| Everything else, together | ~0.1 s |

The proportion is the point. Rust and the bridge are not where the time goes —
Blender is, and it should be. What the architecture is worth is that the other
thirteen calls cost about as much as a hundred milliseconds all told, and that
none of them could have done anything the schema did not describe.

## Doing it faster

Steps 3–7 are independent operations with literal arguments, so they can travel
in a single frame:

```jsonc
{"name": "batch.execute",
 "arguments": {
   "mode": "STOP_ON_ERROR",
   "operations": [
     {"id": "crate", "op": "object.create",
      "args": {"type": "CUBE", "name": "Crate",
               "dimensions": {"x": 0.6, "y": 0.4, "z": 0.35}}},
     {"op": "object.create",
      "args": {"type": "PLANE", "name": "Backdrop",
               "dimensions": {"x": 6, "y": 6, "z": 0}}},
     {"op": "object.transform",
      "args": {"object": {"result_of": "crate"}, "location": {"x": 0, "y": 0, "z": 0.175}}},
     {"op": "modifier.add",
      "args": {"object": {"result_of": "crate"}, "type": "BEVEL", "name": "Edges",
               "properties": [{"name": "width", "value": {"float": 0.008}},
                              {"name": "segments", "value": {"int": 3}}]}}]}}
```

`{"result_of": "crate"}` is resolved structurally against the earlier step's
result — there is no string interpolation anywhere, so one operation can never
smuggle syntax into another's arguments. The two `object.create` calls have no
references and are sent as one frame; the steps that do reference `crate` follow
in order. The reply reports `dispatch_runs`, which is how many frames the batch
actually became.

## Reproducing this

```bash
cargo build --release
python scripts/install_addon.py
blender --background --python scripts/run_bridge.py -- --port 9877 --seconds 300
```

Then run the calls above through any MCP client. The sequence is deterministic:
same inputs, same scene, same framing solve. Only the render time and the UUIDs
will differ.

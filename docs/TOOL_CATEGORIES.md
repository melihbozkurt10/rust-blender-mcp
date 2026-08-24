# Tool categories and lazy loading

247 tool schemas come to roughly 318 KB of JSON. Handing all of that to a model
that wants to move a cube is expensive and, worse, makes the useful tools harder
to find. So the default is lazy: a compact core is visible, and the rest arrive
when asked for.

Both modes exist deliberately. Lazy is better when the client honours
`notifications/tools/list_changed`; eager is better when it does not. Neither is
a workaround the user has to discover on their own.

## Lazy mode (default)

At startup only the `core` category is registered — 10 tools, about 4 KB of
schema:

```
scene.summary            object.list              object.get
selection.get            blender.status           blender.capabilities
tools.categories.list    tools.categories.enable  tools.categories.disable
batch.execute
```

That is enough to look at a scene and decide what else is needed.

```jsonc
// what is available
{"name": "tools.categories.list", "arguments": {}}

// turn on what you need, one call per category
{"name": "tools.categories.enable", "arguments": {"category": "mesh"}}
{"name": "tools.categories.enable", "arguments": {"category": "materials"}}
```

Enabling a category registers its tools and sends
`notifications/tools/list_changed`. Clients that honour it see the new tools
immediately; clients that re-list on their own schedule see them on the next
list.

`tools.categories.disable` removes a category again, which is worth doing when a
long session has accumulated tools it no longer needs. `core` cannot be disabled
— the tools that re-enable the others live there, so turning it off would be a
one-way door, and the attempt is refused with an error that says so.

### Why these ten, and nothing else

`core` is the one category every session pays for, so each member has to earn a
place in every context window there will ever be. The bar is: *a session cannot
get started, or cannot get unstuck, without it.* Anything that fails that bar
belongs in a category, however useful it is.

| Tool | Schema | Why it is unconditional |
| --- | ---: | --- |
| `tools.categories.list` | 0.3 KB | Without it nothing else can be found. |
| `tools.categories.enable` | 0.2 KB | The only door to the other seventeen categories. |
| `tools.categories.disable` | 0.2 KB | Lets a long session give context back. |
| `blender.status` | 0.3 KB | The first thing to call when something is wrong, and the only tool that works before Blender connects. |
| `blender.capabilities` | 0.3 KB | Answers "does this build support that" before a category is loaded to find out the hard way. |
| `scene.summary` | 0.3 KB | The orientation call. Every session starts by asking what is in the file. |
| `object.list` | 1.2 KB | Finding things by name, type, collection or modifier — the usual second step. |
| `object.get` | 0.2 KB | Reading one object once `object.list` has named it. |
| `selection.get` | 0.3 KB | "Do X to what I have selected" is a very common opening instruction. |
| `batch.execute` | 1.0 KB | Calls any tool by name, including from categories that are not enabled, so a session can act efficiently before it has loaded anything. |

The audit that keeps this honest runs the other way round: the question is never
"what else could be useful in core" but "which of these ten could be moved out".
`object.list` is the largest single member at 1.2 KB, more than a quarter of the
4.2 KB total, and so the most scrutinised. It stays because finding objects is a
precondition for almost every task, and the alternative is loading the 28 KB
`scene` category to do it.

Tools from a *disabled* category remain callable by name. A model that remembers
`mesh.bevel` from earlier in the conversation is not punished for the category
having been switched off — the visibility list is an ergonomics feature, not a
security boundary. The security boundary is the handler table.

### Starting with more than core

```bash
BLENDER_MCP_CATEGORIES=mesh,materials,camera blender-mcp
```

`core` is always included whether you list it or not.

## Eager mode

```bash
BLENDER_MCP_EAGER_TOOLS=1 blender-mcp
```

Every category is registered at startup and none can be disabled. Use this when:

- the client does not refresh its tool list on notification;
- the workload spans everything anyway and the context cost is acceptable;
- you are benchmarking, or debugging what the full surface looks like.

In eager mode `tools.categories.enable` and `.disable` still exist but report
that the mode makes them meaningless, rather than silently doing nothing.

## The categories

| Category | Tools | What it is for |
| --- | ---: | --- |
| `core` | 10 | Always on. Orientation, status, category control, batching. |
| `scene` | 40 | Scene settings, objects, collections, selection, snapshots and diffs. |
| `materials` | 9 | Material data-blocks and slot assignment. |
| `shader_nodes` | 12 | Shader graph nodes, links and sockets. |
| `lights` | 6 | Lights and their aiming. |
| `modifiers` | 8 | The modifier stack. |
| `mesh` | 20 | Mesh creation, editing and repair. |
| `animation` | 26 | Keyframes, F-curves, actions, NLA. |
| `geometry_nodes` | 21 | Geometry node groups, graphs and modifiers. |
| `camera` | 11 | Cameras, tracking, depth of field, auto-framing. |
| `render` | 6 | Rendering, screenshots, render settings, artifacts. |
| `import_export` | 5 | Import, export, saving, format capabilities. |
| `uv_texture` | 22 | UV unwrapping, packing, images, baking. |
| `rigging` | 21 | Armatures, bones, constraints, skinning. |
| `rig_diagnostics` | 7 | Rig health checks and their fixes. |
| `assets` | 5 | External asset libraries. |
| `utilities` | 7 | Scene hygiene: cleanup, renaming, orphan purging. |
| `workflows` | 11 | Multi-step operations with rollback. |

`rig_diagnostics` is separate from `rigging` on purpose: checking a rig and
modifying one are different jobs, and a client that only wants to check should
not have to load 21 tools that can change things.

## Choosing categories for a task

| Task | Categories worth enabling |
| --- | --- |
| Look around, understand a file | `core` alone |
| Block out geometry | `scene`, `mesh` |
| Look development | `materials`, `shader_nodes`, `uv_texture` |
| Lighting and camera | `lights`, `camera`, `render` |
| Procedural work | `geometry_nodes` |
| Character setup | `rigging`, `rig_diagnostics`, `animation` |
| Asset pipeline | `import_export`, `assets`, `utilities` |
| "Just do the thing" | `workflows` — one call instead of twenty |

## Cost

Input schema per category, generated from the binary by
`python scripts/check_docs.py --write`:

<!-- generated:tool-categories -->
| Category | Tools | Input schema |
|---|---:|---:|
| `animation` | 26 | 23.8 KB |
| `assets` | 5 | 3.8 KB |
| `camera` | 11 | 10.3 KB |
| `core` | 10 | 4.2 KB |
| `geometry_nodes` | 21 | 60.3 KB |
| `import_export` | 5 | 6.5 KB |
| `lights` | 6 | 7.7 KB |
| `materials` | 9 | 10.2 KB |
| `mesh` | 20 | 20.3 KB |
| `modifiers` | 8 | 13.3 KB |
| `render` | 6 | 6.7 KB |
| `rig_diagnostics` | 7 | 6.4 KB |
| `rigging` | 21 | 26.7 KB |
| `scene` | 40 | 28.5 KB |
| `shader_nodes` | 12 | 41.6 KB |
| `utilities` | 7 | 4.3 KB |
| `uv_texture` | 22 | 21.2 KB |
| `workflows` | 11 | 22.1 KB |
| **Total** | **247** | **317.8 KB** |
<!-- /generated:tool-categories -->

The two graph categories are the expensive ones, because a node plan is a
genuinely rich structure. They are also the two you least often need.

### What a session actually pays

The table above is input schemas alone. A `tools/list` reply also carries names,
titles, descriptions and annotations, so what reaches a client's context is
larger. Measured through the real transport by `python scripts/benchmark.py`:

| Enabled | Tools | `tools/list` bytes | Estimated tokens |
| --- | ---: | ---: | ---: |
| `core` (default) | 10 | 8.1 KB | ~3,476 |
| `core` + `materials` | 19 | 21.3 KB | ~9,791 |
| `core` + `mesh` | 30 | 34.7 KB | ~15,680 |
| `core` + `scene` | 50 | 50.6 KB | ~23,203 |
| `scene`, `mesh`, `modifiers`, `materials` | 87 | 106.3 KB | ~49,180 |
| everything (eager mode) | 247 | 400.8 KB | ~188,519 |

Token counts are estimates from a documented deterministic estimator, not a
model's tokenizer; byte counts are exact. See `benchmarks/README.md`.

Switching a category on mid-session costs 0.40 ms and re-listing afterwards
1.00 ms, so activation is not a reason to load everything up front. Switching one
off returns its tools to the pool and leaves nothing stale in the listing.

## A note on schemas

Schemas are generated with `inline_subschemas`, so no `$ref` or `$defs` reaches
a client. Some MCP clients handle references badly, and a schema that a client
cannot read is worse than a slightly larger one. This costs bytes and buys
compatibility, which is the right trade at these sizes.

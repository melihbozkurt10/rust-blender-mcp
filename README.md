# Rust Blender MCP

**A typed, Rust-first MCP server for Blender with persistent IPC and no
arbitrary Python execution.**

[![CI](https://github.com/melihbozkurt10/rust-blender-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/melihbozkurt10/rust-blender-mcp/actions/workflows/ci.yml)
[![Release](https://github.com/melihbozkurt10/rust-blender-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/melihbozkurt10/rust-blender-mcp/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
[![Blender](https://img.shields.io/badge/Blender-4.2%20LTS%20%E2%80%93%205.1-orange.svg)](https://www.blender.org/)

247 typed tools across 18 categories, each a Rust struct with a JSON schema,
validated before it reaches Blender. The model asks for operations by name; it
never sends code.

---

## Install

No Rust toolchain. No compiling. Two downloads.

1. **Get the release** for your platform from
   [Releases](https://github.com/melihbozkurt10/rust-blender-mcp/releases) —
   a `blender-mcp` binary (≈12.6 MB) and
   `rust-blender-mcp-<version>-blender-extension.zip` (≈137 KB).
2. **Install the extension** in Blender:
   *Edit → Preferences → Add-ons → ▾ → Install from Disk…*, pick the zip, enable
   **Blender MCP Bridge**.
3. **Point your MCP client at the binary**, then press <kbd>N</kbd> in Blender's
   3D viewport, open the **MCP** tab, and press **Connect**.

```json
{
  "mcpServers": {
    "blender": {
      "command": "/absolute/path/to/blender-mcp",
      "args": []
    }
  }
}
```

Ask the assistant to call `blender.status` to confirm. Full instructions,
troubleshooting and every environment variable: [docs/QUICKSTART.md](docs/QUICKSTART.md).

## What makes it different

The obvious way to build a Blender MCP is one tool called `execute_python` that
takes a string and runs it. That version is a few hundred lines and it works —
right up until you want a schema, a validated argument, an error a program can
branch on, a rollback, or any confidence about what a model just did to your
scene.

This is the other version:

- **Typed all the way down.** Every operation is a Rust struct with a generated
  JSON schema, validated before it reaches Blender. A bad call fails with a
  machine-readable error naming the field, not a Python traceback.
- **No arbitrary Python execution.** The server exposes no generic
  code-execution tool: no `execute_python`, no `run_script`, no `shell`, no
  `bpy` expression endpoint. Blender mutations go through explicitly registered
  typed operations, and the set of operations is fixed at compile time on the
  Rust side and at import time on the Python side.
- **One persistent connection.** A length-prefixed frame protocol over loopback
  to a Blender that is already running — not a Blender launch per call.
- **Lazy tool loading.** A session starts with 10 tools and ~8 KB of schema, and
  turns categories on as it needs them, instead of putting 247 tools in the
  context window up front.
- **Batching that is actually faster.** A run of independent operations travels
  in one frame and executes in one main-thread pass. Measured at **10.4× the
  throughput** of the same calls made individually — see below.

## Architecture

```
MCP client
    │  stdio (JSON-RPC)
    ▼
Rust MCP server
    │  typed validation, planning, batching, caching
    │  persistent framed IPC  (loopback TCP, 4-byte length prefix)
    ▼
Blender extension  (Python)
    │  main-thread dispatch: op name → handler, fixed table
    ▼
bpy
```

Requests arrive as MCP tool calls, are deserialised into typed Rust structs and
validated, then sent as a `{op, args}` command over one framed message on
`127.0.0.1`. The add-on looks `op` up in a dictionary built at import time by
explicit decorators, runs the handler on Blender's main thread, and sends back a
typed result or a structured error.

**Why there is Python at all.** `bpy` is Blender's supported integration API and
it exists only inside Blender's own interpreter. There is no way to touch a
`.blend` from another process without either reimplementing Blender or asking
Blender to do it. So this is Rust-*first*, not "100% Rust": the add-on is a small
part of the code, and it decodes a frame, calls `bpy`, and encodes the result.
Every policy decision — schemas, validation, planning, batching, caching,
framing maths, rollback — lives in Rust.

[More →](docs/ARCHITECTURE.md)

## Performance

Every number below was produced by `python scripts/benchmark.py` on the machine
named here. Nothing is extrapolated and nothing is compared against another
project. Full methodology and raw results:
[`benchmarks/`](benchmarks/) · [`benchmarks/results/latest.md`](benchmarks/results/latest.md).

```
CPU      11th Gen Intel Core i7-11800H @ 2.30GHz (8 cores / 16 threads)
RAM      15.8 GB
OS       Windows 10 19045
Blender  5.1.1
Build    cargo build --release
```

| Workload | Result |
| --- | ---: |
| MCP round trip, p50 (server only, no Blender) | **0.116 ms** |
| MCP round trip, p95 | 0.172 ms |
| MCP round trip, p99 | 0.222 ms |
| MCP round trip, throughput | 8,236 req/s |
| Typed Blender operation, p50 (`object.transform`, whole stack) | **1.96 ms** |
| Typed Blender operation, p95 | 2.44 ms |
| 100 individual transforms | 0.20 s (501 ops/s) |
| 100 transforms in one batch | **0.02 s** (~5,000 ops/s) |
| Batch speedup at 100 operations | **10.4×** |
| Batch speedup at 1,000 operations | 11.0× |
| MCP server startup → `initialize` complete | 0.023 s |
| Blender cold launch → bridge connected | 2.78 s |
| Idle server memory (RSS) | 13.1 MB |
| After 1,000 requests | 13.3 MB |
| Default tool schema (`core`) | 10 tools · 8.1 KB · ~3,476 tokens¹ |
| Full tool schema (all categories) | 247 tools · 400.8 KB · ~188,519 tokens¹ |
| Combined download (binary + extension) | 12.7 MB |

¹ Token counts are estimates from a documented deterministic estimator, not a
model's own tokenizer. Byte counts are exact. See
[`benchmarks/harness.py`](benchmarks/harness.py) for the exact rule.

### What these numbers do and do not say

**Rust does not make Blender faster.** It cannot: `bpy` does the work, and a
`bpy` call takes what it takes. `object.create` measures 10.15 ms p50 through the
whole stack, and almost all of that is Blender building a mesh. What a Rust-first
design buys is *orchestration* overhead close to zero — the MCP layer answers in
0.116 ms — and the freedom to coalesce work so that Blender is asked once instead
of a hundred times.

**Most of a single operation's latency is waiting for Blender's main thread.**
The bridge answers on that thread, so a request waits for the next pump tick.
Two measurements pin this down. A bridge round trip that does *no* `bpy` work at
all measures 1.998 ms p50 — statistically the same as the 1.96 ms a real
transform takes. And the same transform inside a batch, where it pays no round
trip of its own, costs 0.18–0.20 ms. So roughly 1.8 ms of every individual call
is the wait, and the transform's own `bpy` work is a tenth of that.

**Batching does not accelerate `bpy`.** It removes round trips. The ceiling is
therefore set by how much of an operation's cost was the round trip — large for a
transform (10.4× at 100 operations, 11.0× at 1,000), small for anything where
Blender itself is the expense.

## Context footprint

Tool schemas are context, and context is the scarcest thing an assistant has. A
session here starts small and grows only into what it uses.

| Enabled categories | Tools | `tools/list` bytes | Estimated tokens¹ |
| --- | ---: | ---: | ---: |
| `core` (default) | 10 | 8.1 KB | ~3,476 |
| `core` + `materials` | 19 | 21.3 KB | ~9,791 |
| `core` + `mesh` | 30 | 34.7 KB | ~15,680 |
| `core` + `scene` | 50 | 50.6 KB | ~23,203 |
| a modelling session (`scene`, `mesh`, `modifiers`, `materials`) | 87 | 106.3 KB | ~49,180 |
| everything | 247 | 400.8 KB | ~188,519 |

Enabling a category mid-session costs 0.40 ms and the client is told its tool
list changed. Disabling one removes those tools again and leaves nothing stale
behind. Tools from a disabled category stay callable by name, so a model that
remembers one is not punished for the category having been turned off.

If your client does not honour `notifications/tools/list_changed`, set
`BLENDER_MCP_EAGER_TOOLS=1` and accept the full schema up front.
[More →](docs/TOOL_CATEGORIES.md)

## Security

The claim is specific, so it is worth stating precisely rather than loosely:

> **The MCP server does not expose a generic Python execution tool. Blender
> mutations are performed through explicitly registered typed operations.**

That is not the same as "safe". It means the attack surface is a finite,
readable list of operations rather than an interpreter. What follows from it:

- **A closed operation set.** Network input selects a handler from a fixed
  dictionary by exact name. It never becomes an attribute path, an operator
  name, or Python source.
- **No dynamic execution in the add-on.** No `eval`, `exec`, `compile`,
  `__import__`, `subprocess` or `os.system` — enforced by an AST scan in
  `scripts/verify_repo.py`, which parses the syntax tree rather than grepping.
- **No child processes.** The server starts no program. Six of the seven crates
  `forbid(unsafe_code)`; the seventh forbids it in everything that ships.
- **Loopback only.** The bridge binds `127.0.0.1` and the server refuses any
  other address without an explicit override.
- **Bounded everything.** Frames have a maximum size, batches a maximum length,
  listings a page cap, downloads a byte ceiling.
- **Managed paths.** Renders, exports, downloads and imports resolve inside
  named roots; a caller supplies a relative path and a root name, never an
  absolute one.
- **Secrets stay out of output.** Credentials live in a `Secret` whose `Debug`
  is redacted.

The asset-provider tools *do* reach the network — that is what they are for.
They fetch from Poly Haven and Sketchfab over HTTPS, size-capped and
extension-checked, into one managed directory, and they report licences rather
than judging them. [More →](docs/SECURITY.md) · [Reporting a vulnerability →](SECURITY.md)

## Features

- **Scene and objects** — hierarchy, collections, selection, visibility,
  transforms, parenting, snapshots and revision diffs.
- **Mesh editing** — primitives, extrude, inset, bevel, subdivide, merge,
  normals, topology repair, geometry analysis.
- **Materials and shader graphs** — Principled BSDF as a typed surface, plus
  generic node graph editing: create, link, set sockets, inspect a tree.
- **Geometry nodes** — node groups, graphs, interfaces, modifier attachment,
  plus ready-made scatter and array-along-curve setups.
- **UVs and texturing** — unwrap, smart project, cube/cylinder/sphere
  projection, seams, packing, image loading, baking.
- **Lighting and cameras** — point/sun/spot/area lights, aiming, lens and depth
  of field, tracking constraints, and auto-framing solved in Rust rather than by
  trial render.
- **Rendering** — engine and sampling settings, stills, viewport captures, and
  artifacts referenced by path instead of inlined as base64.
- **Animation and rigging** — keyframes, F-curves, actions, interpolation, NLA
  strips, armatures, bones, constraints, vertex groups, weights, and read-only
  rig diagnostics.
- **Import/export** — FBX, OBJ, glTF/GLB, USD, STL, PLY, Alembic, Collada and
  SVG, gated by what the running Blender build actually reports.
- **Surface analysis** — a hundred thousand triangles grouped into planar
  regions classified as walls, floors and ceilings, in world space, plus a
  raycast against named objects. One wall comes back as one wall.
- **Asset libraries** — Poly Haven and Sketchfab search, metadata, download and
  import, with licences reported rather than judged.
- **Batching and workflows** — many operations in one round trip with typed
  references between steps and honest atomicity, plus composed workflows such as
  a product turntable or a three-point lighting rig.

## Tool categories

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

That column is the input schemas alone; a `tools/list` reply also carries names,
titles, descriptions and annotations, which is why the context table above shows
400.8 KB for the same 247 tools.

Every tool, with its arguments: [docs/MCP_TOOLS.md](docs/MCP_TOOLS.md). The table
above is generated — `python scripts/check_docs.py --write` refreshes it, and CI
fails if it drifts.

## Usage

Things people actually ask for:

> "Create a studio product scene with a camera and three-point lighting."
>
> "Add a bevel modifier to all selected meshes."
>
> "Create a procedural material and assign it to the active object."
>
> "Build a Geometry Nodes scatter setup on that ground plane."
>
> "Rig this mesh and inspect weight normalization."
>
> "Create a 5-second turntable animation and render it."

What those become on the wire:

```jsonc
// Frame a camera on an object -- solved in Rust, no trial renders
{"name": "camera.auto_frame",
 "arguments": {"camera": "Camera", "objects": ["Product"], "padding": 0.15}}

// A full PBR material -- twelve nodes, fifteen links -- in one call
{"name": "workflow.material.pbr",
 "arguments": {"name": "Concrete",
               "maps": [{"kind": "base_color", "image": "concrete_diff_2k"},
                        {"kind": "roughness",  "image": "concrete_rough_2k"},
                        {"kind": "normal",     "image": "concrete_nor_gl_2k"}],
               "assign_to": ["Floor"]}}

// A three-point rig sized to its subject, rolled back if any part fails
{"name": "workflow.lighting.three_point",
 "arguments": {"target": "Product", "key_energy": 400, "name_prefix": "Studio"}}

// Scatter instances over a surface with geometry nodes
{"name": "geometry_nodes.scatter",
 "arguments": {"surface": "Ground", "instance": "Rock", "density": 4.0,
               "align_to_normal": true, "seed": 7}}

// Bevel two objects in one round trip, undone entirely if either fails
{"name": "batch.execute",
 "arguments": {"mode": "ATOMIC",
               "operations": [
                 {"op": "modifier.add", "args": {"object": "Crate", "type": "BEVEL"}},
                 {"op": "modifier.add", "args": {"object": "Pallet", "type": "BEVEL"}}]}}

// What can something be mounted on? Walls, floors and ceilings, in world space
{"name": "scene.surface.inspect",
 "arguments": {"object": "Warehouse", "classification": "WALL", "min_area": 2.0}}

// Download a CC0 HDRI and make it the world environment
{"name": "asset.import",
 "arguments": {"provider": "polyhaven", "asset_id": "studio_small_08",
               "resolution": 2048, "apply_as_world": true}}
```

A complete worked scene, step by step:
[examples/product_studio.md](examples/product_studio.md). More runnable request
sequences in [`examples/`](examples/).

## Configuration

Everything is an environment variable, set in your MCP client's config block.

| Variable | Default | What it does |
| --- | --- | --- |
| `BLENDER_MCP_PORT` | `9877` | Port the add-on dials into |
| `BLENDER_MCP_HOST` | `127.0.0.1` | Bind address; non-loopback needs `BLENDER_MCP_ALLOW_REMOTE=1` |
| `BLENDER_MCP_EAGER_TOOLS` | unset | `1` registers every tool at startup |
| `BLENDER_MCP_CATEGORIES` | `core` | Categories enabled before the client asks |
| `BLENDER_MCP_WORKSPACE` | per-user data dir | Where renders, exports and downloads land |
| `BLENDER_MCP_REQUEST_TIMEOUT_SECS` | `15` | Deadline for an ordinary request |
| `BLENDER_MCP_MAX_BATCH_OPERATIONS` | `200` | Ceiling on one batch |
| `BLENDER_MCP_SKETCHFAB_TOKEN` | unset | Sketchfab API token; Poly Haven needs none |
| `BLENDER_MCP_LOG` | `info` | Log filter. Diagnostics go to stderr, never stdout |

There is a ready-made config in
[`examples/mcp_client_config.json`](examples/mcp_client_config.json). It works
with any client that speaks MCP over stdio; the shape above is the common one.
No endorsement by any client vendor is implied.

For headless work, `scripts/run_bridge.py` runs the bridge inside a background
Blender:

```bash
blender --background --python scripts/run_bridge.py -- --port 9877 --seconds 600
```

## Building from source

Only needed to develop it. Users download a release.

```bash
git clone https://github.com/melihbozkurt10/rust-blender-mcp
cd rust-blender-mcp
cargo build --release          # binary at target/release/blender-mcp
python scripts/install_addon.py   # copies the add-on into Blender
```

Requires Rust 1.88+ (edition 2024) and Blender 4.2 LTS or newer.

Note that `target/` is a developer build cache and reaches tens of gigabytes. It
is git-ignored and has nothing to do with the 12.7 MB a user downloads.

## Testing

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features                 # 518 tests, no Blender
python scripts/verify_repo.py                         # repository invariants
python tests/protocol/test_error_parity.py            # Rust ↔ Python error codes
python scripts/check_docs.py                          # the numbers above are real

# with Blender installed
blender --background --factory-startup --python scripts/smoke_test.py
blender --background --factory-startup --python tests/blender/test_scene_surface.py
python tests/blender/test_standalone_smoke.py         # a stock Blender, from scratch
python tests/blender/test_bridge_roundtrip.py         # the whole stack
python tests/blender/test_asset_import.py             # + the network
```

[More →](docs/TESTING.md)

## Limitations

- **Blender must be running** with the extension connected. There is no headless
  "just parse the .blend" path; everything goes through `bpy`.
- **One Blender at a time.** The server talks to a single connected bridge.
- **Blender 4.2 LTS and newer.** Developed against 4.2 and 5.1; 3.x will not work.
- **No arbitrary Python, deliberately.** If an operation is not in the tool list,
  it cannot be performed through this server. That is the trade the design makes,
  and the answer is a new typed tool rather than an escape hatch.
- **Atomic batches cover `.blend` mutation only.** They use Blender's undo stack,
  so anything that writes outside the file — a render, an export, a download — is
  refused from an atomic batch rather than pretended over. Undo also needs a
  Blender UI, so atomic batches are unavailable in background mode.
- **Young project.** The protocol is at version 1 and interfaces may still move.

## Contributing

Bug reports, tools and operations are all welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the checks that must pass and how the
three sides of an operation fit together.

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

The whole repository is Apache-2.0: the Rust crates, the Blender extension, the
scripts and the docs. Apache-2.0 is compatible with Blender's GPL-3.0-or-later,
so the extension may carry it; a combined work with Blender is governed by
Blender's licence. No third-party source is vendored here.

Assets downloaded through the asset-provider tools are licensed by the provider
that served them, not by this licence.

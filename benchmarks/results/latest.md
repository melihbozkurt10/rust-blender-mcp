# Benchmark results

Generated 2026-08-24 19:28:37Z · rust-blender-mcp 0.1.0 (uncommitted)

**Published baseline for v0.1.0**

## Environment

| | |
|---|---|
| CPU | 11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz |
| Cores / threads | 8 / 16 |
| RAM | 15.8 GB |
| OS | Windows 10 (10.0.19045) |
| Blender | Blender 5.1.1 |
| Build profile | release |
| Python (harness) | 3.14.2 |

## MCP round trip (no Blender)

Path: `MCP client -> stdio -> Rust server -> handler -> back (no Blender IPC)`

**`blender.status`, 10000 warm requests**

| samples | min | p50 | p95 | p99 | max | mean | req/s |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 10000 | 0.069 ms | 0.116 ms | 0.172 ms | 0.222 ms | 0.894 ms | 0.121 ms | 8236 |

Client-side JSON encode/decode inside the harness: 0.0062 ms per request, included in the figures above.

## IPC floor (no Rust server, no bpy work)

Path: `benchmark harness -> framed socket -> inbox -> main-thread pump -> dispatch -> back`  
Excludes: the Rust MCP server and any bpy work

Main-thread pump cadence: 1 ms while a session is active, 8 ms after 500 ms of quiet.

**`system.ping`**

| samples | min | p50 | p95 | p99 | max | mean | req/s |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 2000 | 0.973 ms | 1.998 ms | 2.202 ms | 2.365 ms | 4.888 ms | 1.999 ms | 500 |

## Startup

Server-ready is spawn to a completed MCP `initialize`. Blender-connect is a cold `blender --background --factory-startup` launching the bridge and completing the socket handshake; most of it is Blender's own start-up, not the bridge's.

| stage | samples | p50 | p95 | max |
|---|---:|---:|---:|---:|
| MCP server spawn → initialized | 7 | 0.023 s | 0.023 s | 0.023 s |
| Blender launch → bridge connected | 7 | 2.779 s | 2.995 s | 2.995 s |
| Spawn → ready to operate | 7 | 2.801 s | 3.018 s | 3.018 s |
| First `blender.capabilities` | 7 | 0.001 s | 0.001 s | 0.001 s |

## Blender operations, full stack

Path: `MCP client -> Rust server -> framed IPC -> Blender main thread -> bpy -> back`

Each figure is one whole round trip. It is not split into server / IPC / bpy components because the bridge does not timestamp the stages, and a split that is not measured would be a guess. Compare against `bridge_floor`, which is the same path with the bpy work removed.

| operation | samples | p50 | p95 | p99 | ops/s |
|---|---:|---:|---:|---:|---:|
| `scene.statistics` | 200 | 2.00 ms | 2.34 ms | 2.49 ms | 500 |
| `object.transform` | 200 | 1.96 ms | 2.44 ms | 2.74 ms | 497 |
| `object.create` | 100 | 10.15 ms | 14.30 ms | 16.49 ms | 100 |
| `modifier.add` | 100 | 1.95 ms | 2.42 ms | 2.57 ms | 506 |
| `material.assign` | 100 | 1.99 ms | 2.31 ms | 2.45 ms | 500 |

## Sequential operations

`object.transform` — One MCP tool call per transform: N request/response round trips.

| operations | total | ops/s | p50 | p95 |
|---:|---:|---:|---:|---:|
| 100 | 0.20 s | 501 | 2.02 ms | 2.40 ms |
| 500 | 1.00 s | 500 | 1.99 ms | 2.38 ms |
| 1000 | 2.00 s | 500 | 1.99 ms | 2.35 ms |

## Batch vs individual

`speedup` above 1 means the batch was faster. Batching removes per-call round trips; it does not make the underlying bpy call any faster, so the ceiling is set by how much of a call's cost was the round trip.

| operations | individual | batched | speedup | per-op individual | per-op batched |
|---:|---:|---:|---:|---:|---:|
| 10 | 0.02 s | 0.00 s | 4.93× | 1.99 ms | 0.40 ms |
| 100 | 0.21 s | 0.02 s | 10.4× | 2.05 ms | 0.20 ms |
| 500 | 1.00 s | 0.09 s (3 chunks) | 10.85× | 2.01 ms | 0.18 ms |
| 1000 | 2.01 s | 0.18 s (5 chunks) | 11.0× | 2.01 ms | 0.18 ms |

## Tool schema / context footprint

Measured: the exact JSON of an MCP `tools/list` reply, as a client receives it.  
Token source: `estimate` — `estimate` is the documented deterministic estimator in benchmarks/harness.py, not a model tokenizer. It is an estimate; byte counts are exact.

| categories | tools | schema | tokens | `tools/list` p50 |
|---|---:|---:|---:|---:|
| core | 10 | 8.1 KB | 3,476 | 0.179 ms |
| core+scene | 50 | 50.6 KB | 23,203 | 0.760 ms |
| core+mesh | 30 | 34.7 KB | 15,680 | 0.558 ms |
| core+materials | 19 | 21.3 KB | 9,791 | 0.391 ms |
| modelling_session | 87 | 106.3 KB | 49,180 | 1.833 ms |
| all | 247 | 400.8 KB | 188,519 | 8.396 ms |

Enabling a category mid-session: 0.40 ms, re-listing afterwards 1.00 ms. Disabling removes the tools again (30 → 10): no stale tools left behind.

## Memory (MCP server process)

Metric: resident set size; on Windows this is the process working set on `win32`.

| point | RSS |
|---|---:|
| startup | 12.7 MB |
| idle | 13.1 MB |
| after 1000 requests | 13.3 MB |
| after batch | 14.2 MB |

## Distribution size

| item | size |
|---|---:|
| MCP server binary (`target/release/blender-mcp.exe`) | 12.6 MB |
| Blender extension (`blender_mcp_bridge-0.1.0.zip`) | 136.7 KB |
| **Combined download** | **12.7 MB** |
| Source tree (no `.git`, no `target/`) | 2.1 MB |
| Developer build cache `target/` | 12.1 GB |

_A developer's Rust build cache. It is git-ignored, it is never published, and it has nothing to do with what a user downloads._


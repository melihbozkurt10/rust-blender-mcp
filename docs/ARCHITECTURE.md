# Architecture

A model asks for "a three-point lighting rig around the product". Something has
to turn that into key, fill and rim lights at computed positions, with computed
energies, parented into a collection, and undo the lot if the third one fails.
That "something" is the point of this project, and it lives in Rust.

The alternative — an MCP server that accepts a string of Python and runs it in
Blender — is a shorter program and a worse one. It has no schema, no validation,
no error taxonomy, no rollback, no capability negotiation, and it hands arbitrary
code execution to whatever is on the other end of the socket. This design refuses
that trade in the strongest available way: **there is no code path from network
input to executed code.** See [SECURITY.md](SECURITY.md).

## The shape of it

```
MCP client (Claude, an IDE, a script)
        │  JSON-RPC over stdio
        ▼
┌──────────────────────────────────────────────────────────┐
│ blender-mcp-server                                       │
│   tool registry ── lazy categories, JSON schemas         │
│   domain logic  ── geometry, framing, lighting, graphs   │
│   workflows     ── multi-step runs with rollback         │
│   scene cache   ── revisions, diffs, invalidation        │
│   artifacts     ── managed files, no inline base64       │
└──────────────────────────────────────────────────────────┘
        │  length-prefixed JSON over loopback TCP
        ▼
┌──────────────────────────────────────────────────────────┐
│ blender_extension (the add-on, ~5% of the code)          │
│   reader thread → queue → main-thread pump → HANDLERS    │
│   HANDLERS: a fixed dict of name → function              │
└──────────────────────────────────────────────────────────┘
        │  bpy / bmesh, on Blender's main thread
        ▼
     Blender
```

The add-on is deliberately thin. It decodes a frame, looks a name up in a fixed
table, calls the function, and encodes the result. It contains no MCP, no
planning, no policy, and no way to reach anything that is not in that table.

## The crates

| Crate | What it owns |
| --- | --- |
| `blender-protocol` | The contract: ids, math, errors, envelopes, handshake, capabilities, and a typed payload per domain. No I/O. |
| `blender-client` | The socket: framing, pending-request correlation, sessions, reconnection, per-operation timeouts. |
| `blender-domain` | The maths: wall runs, camera framing solves, lighting rigs, PBR/glass/emissive graph plans, scatter and array plans, export validation. |
| `workflow-engine` | Multi-step runs: steps, compensating actions, rollback, a recording executor so every workflow is testable without Blender. |
| `scene-cache` | What changed and when: a bounded revision history, change folding, invalidation, snapshots. |
| `asset-providers` | External libraries behind one trait, with the download policy and credential handling in one place. |
| `mcp-server` | The MCP surface: tool registry, schemas, category activation, artifacts, configuration, and the binary. |

Dependencies point one way: `mcp-server` knows about everything, everything
knows about `blender-protocol`, and `blender-protocol` knows about nothing.

## Why Rust holds the model

Everything that can be decided without Blender is decided without Blender.

- **Validation** happens before a request is sent. A negative focal length, an
  inverted frame range, a roughness of 1.4 and an unknown enum are all rejected
  in Rust, with an error that names the field and lists the valid values. The
  round trip is not spent discovering that the input was wrong.
- **Planning** happens in Rust. `workflow.lighting.three_point` computes
  positions and energies from a spec and sends light creations. A shader graph
  is a `GraphPlan` — nodes, sockets, links, positions — built in Rust and applied
  by one bridge operation. Blender is asked to *do* things, not to *decide*
  them.
- **State** is tracked in Rust. The cache folds events into a revision history so
  `scene.diff` can answer "what changed since I last looked" without re-reading
  the scene.

The add-on's job is the part that genuinely requires `bpy`.

## Threading, and why Blender does not freeze

`bpy` is not thread-safe, and a blocking read on Blender's main thread would
freeze the UI. So:

1. A **reader thread** owns the socket. It blocks on `recv`, reassembles frames,
   and pushes decoded requests onto a queue.
2. A **timer** registered with `bpy.app.timers` runs the **pump** on the main
   thread. It drains the queue, dispatches each request, and pushes results onto
   an outgoing queue.
3. A **writer** sends replies back.

The reader never touches `bpy`; the pump never blocks on the network. Long
operations still take as long as they take — a 4K render blocks the main thread
because Blender blocks the main thread — but nothing is blocked *waiting on the
network*.

### The pump interval is the latency

A request cannot be answered before the next pump tick, so whatever interval the
pump asks for is, near enough, the round-trip latency of every operation.
Measurement puts the rest of the bridge — socket, framing, JSON, dispatch — at
about 0.15 ms, which is nothing beside it.

One fixed interval would have to choose between a responsive bridge and a timer
that never wakes for nothing, so the pump does not use one. It returns:

| Situation | Next tick |
| --- | ---: |
| Work already queued | 1 ms |
| Something handled within the last 500 ms | 1 ms |
| Otherwise | 8 ms |

A session that is actively driving the bridge therefore runs at 1 kHz, and one
that has been abandoned settles to 125 Hz, where each tick is a single
non-blocking queue read that finds nothing. `blender_extension/config.py` holds
the three constants and `dispatcher.next_interval` the rule; `scripts/smoke_test.py`
checks all three branches.

### Coalescing a batch

Because a round trip costs a tick, sending a batch one operation at a time costs
one tick per operation — which made `batch.execute` no faster than the same calls
made individually. It does not do that any more.

The server groups consecutive batch operations that are plain forwards with
fully literal arguments, validates each one in Rust, and sends the whole run as a
single `batch.dispatch` frame. The add-on runs the run inside one pump pass
against the same fixed handler table a single request uses. A step whose tool
does work in Rust, or whose arguments still hold a `{"result_of": …}` reference,
ends the run; the ordinary one-at-a-time path picks it up and the next run starts
after it.

Nothing about validation moves to the Python side: a run reaches Blender only
after every operation in it has been decoded and validated against its tool's
schema. Measured at 100 transforms, this is about ten times the throughput of the
same operations sent individually — see `benchmarks/results/latest.md`.

## Ids that survive renaming

Blender identifies data by name, and names change. Every entity this server hands
out carries a UUID stored in a custom property (`object["mcp_id"]`), so a
reference stays valid across renames, and a name is only ever a fallback lookup.
Ids are phantom-typed in Rust (`Id<ObjectKind>`, `Id<MaterialKind>`), so a
material id cannot be passed where an object id belongs — a class of bug that
simply cannot compile.

Where Blender refuses custom properties — modifiers, for instance — the add-on
uses the intrinsic identifier Blender already provides (`persistent_uid`) rather
than inventing a parallel scheme.

## Capability negotiation, not version sniffing

Blender 4.2 and 5.x differ in ways no version check predicts. The add-on reports
what the running build actually has — render engines, modifier types, node types,
export formats — during the handshake, and the server validates against that.

This is not theoretical. Blender 5.1 dropped the `WORKBENCH` render engine. No
code change was needed: the capability list simply stopped containing it, and a
request for it now fails with `CAPABILITY_UNAVAILABLE` naming the engines that do
exist. Similarly, Blender 4.4's slotted layered Actions removed `Action.fcurves`;
the add-on has one compatibility module that finds F-curves either way, and the
26 animation tools do not care which Blender they are talking to.

## Errors are data

Every failure is an `ErrorCode` from a closed taxonomy plus a machine-readable
`details` object. An unknown node socket comes back with the sockets that do
exist. An expired revision comes back with the oldest one still answerable. A
missing credential comes back with the name of the environment variable to set.

No caller ever has to parse prose to find out what went wrong, and a model can
usually correct itself on the next call. See [PROTOCOL.md](PROTOCOL.md#errors).

## What is deliberately absent

- Any tool that executes Python, shell, or operating-system commands.
- Inline base64 for large payloads — renders and exports are returned as managed
  artifact references.
- Unbounded listings — every listing operation paginates.
- Unbounded history — the revision cache holds a fixed window and says
  `REVISION_EXPIRED` rather than answering incompletely.
- A "free to use" flag on downloaded assets — licences are reported exactly as
  the provider states them.

## Further reading

- [PROTOCOL.md](PROTOCOL.md) — the wire format, handshake and error taxonomy
- [SECURITY.md](SECURITY.md) — the execution boundary and what enforces it
- [TOOL_CATEGORIES.md](TOOL_CATEGORIES.md) — lazy loading and the category set
- [TRANSACTIONS.md](TRANSACTIONS.md) — batching, undo and rollback
- [NODE_GRAPHS.md](NODE_GRAPHS.md) — how shader and geometry graphs are built
- [TESTING.md](TESTING.md) — what is tested, and how, without a GPU

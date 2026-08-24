# Batching, transactions and rollback

Three mechanisms, with different honesty about what they can guarantee.

| | `batch.execute` | Workflows | Single tools |
| --- | --- | --- | --- |
| Steps | Whatever you list | Fixed, planned in Rust | One |
| References between steps | Typed `result_of` | Internal | n/a |
| Undo-backed atomicity | `ATOMIC` mode, `.blend` writes only | no | no |
| Compensating rollback | no | yes | no |
| External side effects | Refused in `ATOMIC` | Never rolled back | Allowed |

## `batch.execute`

Runs several tool calls in one round trip. The tools are the same ones you would
call individually, with the same schemas, validated the same way.

```jsonc
{
  "mode": "ATOMIC",
  "operations": [
    {"id": "base", "op": "object.create",
     "args": {"type": "CUBE", "name": "Base"}},

    {"op": "object.transform",
     "args": {"object": {"result_of": "base"}, "location": {"x": 0, "y": 0, "z": 0.5}}},

    {"op": "modifier.add",
     "args": {"object": {"result_of": "base"}, "type": "BEVEL", "width": 0.02}}
  ]
}
```

`mode` is `BEST_EFFORT`, `STOP_ON_ERROR` (the default) or `ATOMIC`.

### References are structural

`{"result_of": "base"}` is a typed marker, not a template. The argument structure
is **walked** and matching objects are replaced with the referenced value —
nowhere is a string interpolated into another string. A reference can sit
anywhere: nested in an object, inside an array, as one element of a vector. A
string that merely looks like a reference is left alone, because the match is on
shape, not on text.

With no `path`, the reference resolves to the conventional identifier of the
earlier result — results in this server put what they created under a well-known
key, so `{"result_of": "base"}` means "the object the `base` step created". Give
an explicit `path` when you want something else:

```jsonc
{"result_of": "base", "path": "object.name"}
```

An unresolvable reference — an unknown id, a step that failed, a path that is not
present — fails that operation with `INVALID_ARGUMENT` naming the reference. It
never silently becomes null.

### Modes

**`BEST_EFFORT`** — run everything, report each failure, carry on. For
independent work where partial progress is useful.

**`STOP_ON_ERROR`** (default) — stop at the first failure. Everything before it
stays applied. The response says exactly where it stopped and what had already
run.

**`ATOMIC`** — stop at the first failure and undo everything the batch did, using
Blender's undo stack.

### What `ATOMIC` really means

It means one `bpy.ops.ed.undo_push` before the batch and, on failure, undo back
to that point. That covers `.blend` data mutation, which is what the undo system
covers.

It does not cover anything outside the file. So `ATOMIC` **refuses up front** if
any operation is classified `EXTERNAL_SIDE_EFFECT`:

```jsonc
{
  "code": "TRANSACTION_UNSUPPORTED",
  "message": "`render.execute` writes outside the .blend file, so it cannot take part in an atomic batch -- undoing would not remove what it wrote. Use STOP_ON_ERROR, or run it outside the batch.",
  "details": {"index": 2, "op": "render.execute"}
}
```

An unwritten file cannot be un-written; pretending otherwise would be the most
dangerous kind of convenience.

`ATOMIC` also refuses when Blender is running headless, because a background
Blender has no undo stack. The error says so rather than running the batch and
hoping:

```jsonc
{
  "code": "TRANSACTION_UNSUPPORTED",
  "message": "This Blender is running in background mode, which has no undo stack. Atomic batches need one."
}
```

If the undo itself fails, the code is `ROLLBACK_FAILED` and the response reports
what had been applied. That is a bad day, and it is reported as one rather than
smoothed over.

### Validation happens first

The whole batch is validated before **any** of it runs: every tool name must
exist, every argument set must satisfy its schema, every `result_of` must name an
earlier step, and in `ATOMIC` mode every operation must be undo-able. A batch
that cannot possibly succeed does not get half-applied first.

Batch size is capped at 200 operations (`BLENDER_MCP_MAX_BATCH_OPERATIONS`).

### Why a batch is faster, and by how much

A round trip to Blender costs one main-thread pump tick, and a typed operation
usually costs far less than a tick. Sent one at a time, a batch of 100 therefore
spends nearly all of its time waiting rather than working — which is why, before
this was fixed, batching was measurably *slower* than the same 100 calls made
individually, once its own bookkeeping was counted.

The server now groups consecutive operations that are plain forwards to Blender
with fully literal arguments, and sends each group as one `batch.dispatch` frame
that the add-on runs inside a single pump pass. A step is left out of a group
when its tool does work in Rust rather than forwarding, or when its arguments
still hold a `{"result_of": …}` reference — that value does not exist until the
previous step's frame comes back. Grouping resumes after it.

Validation does not move: every operation in a group is decoded and checked
against its tool's schema in Rust before the frame is sent. The response reports
`dispatch_runs`, so a caller can see how many frames a batch actually became.

Measured on the reference machine, 100 `object.transform` calls take 0.20 s
individually and 0.02 s as one batch — 10.4 times the throughput. The
ceiling is set by how much of an operation's cost was the round trip, so an
operation where Blender itself is the expense gains much less. See
`benchmarks/results/latest.md`.

### The response

```jsonc
{
  "success": true,
  "mode": "ATOMIC",
  "completed": 3,
  "total": 3,
  "results": [
    {"index": 0, "id": "base", "op": "object.create", "ok": true, "result": { … }},
    {"index": 1, "id": null,   "op": "object.transform", "ok": true, "result": { … }},
    {"index": 2, "id": null,   "op": "modifier.add", "ok": true, "result": { … }}
  ]
}
```

On failure the payload also carries `failed_index`, and — in `ATOMIC` mode —
`rolled_back`, plus `rollback_error` if the undo itself did not work.

## Workflows

A workflow is a fixed sequence planned in Rust: `workflow.lighting.three_point`
computes positions and energies, then creates a collection and three lights.

Workflows roll back with **compensating actions**, not undo. Each step that
creates something registers how to remove it; on failure the compensations run in
reverse order. This works headless, and it works when a step is not undo-able —
but it is a best effort, not a transaction. A compensation that itself fails is
reported.

```jsonc
{
  "workflow": "workflow.lighting.three_point",
  "status": "failed",
  "steps": [
    {"name": "plan the rig", "ok": true, "detail": {"lights": 3, "distance": 4.2}},
    {"name": "create a collection for the lights", "ok": true, "op": "collection.create"},
    {"name": "create the key light", "ok": true, "op": "light.create"},
    {"name": "create the fill light", "ok": false, "op": "light.create",
     "error": {"code": "INVALID_ARGUMENT", "message": "`energy` must be positive."}}
  ],
  "rolled_back": true,
  "compensations": [
    {"op": "light.delete", "ok": true},
    {"op": "collection.delete", "ok": true}
  ]
}
```

Set `rollback_on_failure: false` to keep what succeeded — useful when you want to
inspect a partial result rather than lose it.

Every step is reported whether or not anything failed, so a workflow is never a
black box. A workflow that cannot track what it created **fails loudly** rather
than continuing with an untrackable object: silently skipping a compensation
would turn "rolled back" into a lie.

## Choosing

- **One thing** → call the tool.
- **Several related things, all `.blend` mutations, and half-done would be bad**
  → `batch.execute` with `mode: "ATOMIC"`.
- **Several independent things** → `batch.execute` with `mode: "BEST_EFFORT"`.
- **A known composite operation** → the workflow, which has already thought about
  the ordering and the rollback.
- **Anything touching the filesystem or the network** → run it on its own and
  handle failure yourself. Nothing here can undo it.

## Revisions

Mutating bridge operations report the scene revision after the change. Combined
with `scene.snapshot` and `scene.diff`, that is how you find out what a batch or
a workflow actually did:

```jsonc
{"name": "scene.snapshot"}                            // → {"revision": 41, …}
{"name": "batch.execute", "arguments": { … }}
{"name": "scene.diff", "arguments": {"from_revision": 41}}
```

The diff folds changes, so an object created and then edited five times appears
once. A revision that has fallen out of the history window (1000 by default)
returns `REVISION_EXPIRED` with the oldest revision still answerable, rather than
an answer that looks complete and is not.

# Benchmarks

What this measures, how, and what the numbers do not mean.

```bash
cargo build --release
python scripts/benchmark.py                 # everything it can run
python scripts/benchmark.py --no-blender    # only the suites that need no Blender
python scripts/benchmark.py --quick         # a tenth of the samples, for a sanity check
python scripts/benchmark.py --only batch sequential
```

Results are written to `benchmarks/results/latest.json` (structured) and
`latest.md` (readable). `psutil` is needed for the memory suite; everything else
is standard library.

## The principle

Benchmarks observe the product **from outside**, through the same stdio MCP
transport a real client uses. Nothing in `crates/` or `blender_extension/`
imports anything here, so a measurement can never become a runtime dependency
and instrumentation can never distort what it measures.

The corollary is that every figure includes the client's own JSON encoding in
CPython. That cost is measured separately and reported, and it is left in the
headline numbers because a real client always pays it.

## The three vantage points

The most important thing in the results is which layer a number describes.
Nothing is derived by subtracting one from another.

| Suite | Path | Excludes |
| --- | --- | --- |
| `mcp_roundtrip` | client → stdio → Rust server → handler → back | Blender entirely |
| `bridge_floor` | harness → framed socket → pump → dispatch → back | the Rust server, and any `bpy` work |
| `blender_ops` | the whole stack | nothing |

`mcp_roundtrip` uses `blender.status`: a real registered tool, with a real
schema and a real handler, that answers from server state without crossing the
bridge. Blender's contribution is held at zero rather than estimated away.

`bridge_floor` uses `system.ping`, a bridge operation that returns a constant.
It is deliberately **not** an MCP tool and is not exposed to a model; it exists
for liveness checks. The harness speaks the frame protocol directly, which is
why no Rust is in that path.

Neither of these is a fake production tool added for benchmarking. Adding one
would put a meaningless entry in every user's tool list.

## Suites

| Name | What it answers |
| --- | --- |
| `mcp_roundtrip` | What does the MCP layer cost, with Blender held out? 10,000 warm requests. |
| `startup` | How long from spawn to a working session, in the four stages a user waits through? |
| `bridge_floor` | What does one IPC round trip cost with no `bpy` work at all? |
| `blender_ops` | What does a real typed operation cost end to end? Five different operations. |
| `sequential` | 100 / 500 / 1000 individual transforms: total time and ops/s. |
| `batch` | The same workloads through `batch.execute`, against the individual path. |
| `context_footprint` | Tools, bytes and estimated tokens for six category combinations. |
| `memory` | Server RSS at startup, idle, after 1,000 requests, after a batch. |
| `distribution` | Binary size, extension size, combined download, source tree, build cache. |

## Token estimates

`context_footprint` reports both bytes and tokens. **Bytes are exact** — they are
the length of the actual `tools/list` reply. **Tokens are an estimate** and are
labelled as one everywhere they appear.

If `tiktoken` is importable the suite uses it and reports the source as
`tiktoken:cl100k_base`. Otherwise it uses the deterministic estimator in
`harness.py`, whose rule is written out in full in that function's docstring:
byte-pair-style pre-tokenisation, then four characters per token for letter runs,
three for digit runs, one per character for punctuation. It exists so the numbers
are reproducible on a machine with no network access and no vendor tokenizer.

A byte count is never presented as a token count.

## Reading the results honestly

**Rust does not make Blender faster.** `bpy` does the work. `object.create`
measures around 9.6 ms end to end and almost all of that is Blender building a
mesh. What the architecture buys is orchestration overhead near zero and the
ability to coalesce work.

**A single operation's latency is mostly the main-thread handshake.** The bridge
answers on Blender's main thread, so a request waits for the next pump tick. The
socket, framing and JSON together are about 0.15 ms. Compare `bridge_floor`
against `blender_ops` to see how much of an operation is Blender and how much is
the wait.

**Batch speedup has a ceiling set by that ratio.** Batching removes round trips;
it does not accelerate `bpy`. A transform is nearly all round trip, so it gains
about tenfold. Something where Blender is the expense gains much less. If a
workload is slower batched, the suite reports that.

**`target/` is not the application.** The `distribution` suite reports the
developer build cache separately and says so. It is tens of gigabytes, it is
git-ignored, and it has nothing to do with what a user downloads.

## Reproducing a published result

Every result file records the CPU, core count, RAM, OS, Blender version, build
profile, project version and git commit. A result without those is an anecdote.

The committed `results/latest.md` is the published baseline for the version in
its header, measured on the machine named in it. Numbers from a different machine
will differ; the shape of the relationships should not.

## Adding a suite

A suite is a function taking a `Context` and returning a JSON-ready dict, listed
in `SUITES` at the bottom of `suites.py`. It should skip with a reason rather
than fail when it cannot run, and it should say next to each measurement what
that measurement excludes.

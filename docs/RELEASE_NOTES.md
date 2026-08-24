# v0.1.0 — first public release

A typed, Rust-first MCP server for Blender with persistent IPC and no arbitrary
Python execution.

**247 typed tools** across **18 categories**, backed by **234 bridge
operations** and **47 typed error codes**. Every operation is a Rust struct with
a generated JSON schema, validated before anything reaches Blender. There is no
`execute_python`, no `run_script`, no shell, and no `bpy` expression endpoint.

## Install

No Rust toolchain required.

1. Download the binary for your platform below and put it somewhere permanent.
2. Download `rust-blender-mcp-0.1.0-blender-extension.zip` and install it in
   Blender: *Edit → Preferences → Add-ons → ▾ → Install from Disk…*, then enable
   **Blender MCP Bridge**.
3. Point your MCP client at the binary:

   ```json
   {
     "mcpServers": {
       "blender": { "command": "/absolute/path/to/blender-mcp", "args": [] }
     }
   }
   ```

4. In Blender, press <kbd>N</kbd> in the 3D viewport, open the **MCP** tab, and
   press **Connect**.

Ask the assistant to call `blender.status` to confirm. Full instructions and
troubleshooting: [docs/QUICKSTART.md](https://github.com/melihbozkurt10/rust-blender-mcp/blob/main/docs/QUICKSTART.md).

| Platform | File |
| --- | --- |
| Windows x86_64 | `rust-blender-mcp-0.1.0-windows-x86_64.zip` |
| Linux x86_64 | `rust-blender-mcp-0.1.0-linux-x86_64.tar.gz` |
| macOS Apple Silicon | `rust-blender-mcp-0.1.0-macos-aarch64.tar.gz` |
| macOS Intel | `rust-blender-mcp-0.1.0-macos-x86_64.tar.gz` |
| Blender extension | `rust-blender-mcp-0.1.0-blender-extension.zip` |

Verify with `SHA256SUMS.txt`.

Download sizes: 3.88 MB (macOS Apple Silicon), 4.13 MB (macOS Intel), 4.26 MB
(Linux), 4.79 MB (Windows), plus 136 KB for the extension. Unpacked, the binary
is 15.4 MB.

The macOS binaries are unsigned; remove the quarantine attribute with
`xattr -d com.apple.quarantine ./blender-mcp`. The Linux binary targets glibc
2.35 and newer (Ubuntu 22.04+).

## Tested against

Blender **4.2 LTS** and **5.1**. Blender 3.x will not work.

## What is in it

- **Typed tools, not code.** Modelling, materials, shader and geometry node
  graphs, UVs, lighting, cameras, rendering, animation, rigging, import/export,
  surface analysis, external asset libraries, batching and multi-step workflows.
- **Lazy tool loading.** A session starts with 10 core tools and about 8 KB of
  schema, and turns categories on as it needs them. Loading everything is 247
  tools and 400 KB. Both modes are supported deliberately —
  `BLENDER_MCP_EAGER_TOOLS=1` for clients that do not refresh their tool list.
- **A persistent bridge.** One length-prefixed frame protocol over loopback to a
  Blender that is already running, with automatic reconnection.
- **Batch execution.** Independent operations travel in one frame and run in one
  main-thread pass — measured at 10.4× the throughput of the same calls made
  individually. Typed `{"result_of": …}` references pass a result from one step
  to another with no string interpolation anywhere.
- **Honest atomicity.** `ATOMIC` batches use Blender's undo stack, which really
  does revert `.blend` mutations. Operations that write outside the file are
  refused from an atomic batch rather than pretended over.
- **Structured errors.** 47 codes shared by the Rust and Python sides and checked
  for parity by a test. An error names the field, gives the value and lists the
  alternatives.

## Measured performance

On an Intel i7-11800H, Windows 10, Blender 5.1.1, release build. Reproduce with
`python scripts/benchmark.py`; full results in
[`benchmarks/results/latest.md`](https://github.com/melihbozkurt10/rust-blender-mcp/blob/main/benchmarks/results/latest.md).

| | |
| --- | ---: |
| MCP round trip p50 / p95 / p99 (no Blender) | 0.116 / 0.172 / 0.222 ms |
| Typed Blender operation p50 (`object.transform`) | 1.96 ms |
| 100 individual transforms | 0.20 s |
| 100 transforms in one batch | 0.02 s (**10.4×**) |
| Server startup to `initialize` | 0.023 s |
| Idle server memory | 13.1 MB |
| Download, archive + extension | 4.0–4.9 MB |

Rust does not make Blender faster — `bpy` does the work and takes what it takes.
What it buys is orchestration overhead near zero and the ability to coalesce work
so Blender is asked once rather than a hundred times.

## Verification

Every check below passes on the tagged tree:

- 518 Rust tests (`cargo test --workspace --all-features`)
- 497 in-Blender operation checks (`scripts/smoke_test.py`)
- 27 scene-surface checks, 71 end-to-end stack checks, 17 standalone smoke
  checks, 36 asset-pipeline checks
- Protocol parity between Rust and Python across all 47 error codes
- `cargo fmt --check`, `cargo clippy -D warnings`, `scripts/verify_repo.py`
- The add-on packages reproducibly: the same source always produces the same
  archive bytes

## Security posture

The MCP server does not expose a generic Python execution tool. Blender
mutations are performed through explicitly registered typed operations. That is
not a claim of safety; it means the surface is a finite readable list rather than
an interpreter.

- No `eval`, `exec`, `compile`, `__import__`, `subprocess` or `os.system` in the
  add-on — enforced by an AST scan, not a grep.
- No child processes. Loopback bind only. Bounded frames, batches, listings and
  downloads. Paths confined to managed roots. Credentials in a redacting wrapper.
- The asset-provider tools do reach the network, on purpose, over HTTPS to Poly
  Haven and Sketchfab. `BLENDER_MCP_ALLOW_ASSET_DOWNLOADS=0` turns that off.

See [SECURITY.md](https://github.com/melihbozkurt10/rust-blender-mcp/blob/main/SECURITY.md) and [docs/SECURITY.md](https://github.com/melihbozkurt10/rust-blender-mcp/blob/main/docs/SECURITY.md), which
also list the known limits rather than hiding them.

## Licence

Apache-2.0, consistently: the Rust crates, the Blender extension, the scripts and
the docs. No third-party source is vendored.

## Known limitations

- Blender must be running with the extension connected; there is no headless
  "parse the .blend" path.
- One Blender at a time.
- Atomic batches need a Blender UI, because that is where the undo stack lives.
- The protocol is at version 1 and interfaces may still move before 1.0.

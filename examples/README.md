# Examples

Worked sequences of real tool calls. Every argument here matches a schema in the
server — these are transcripts, not pseudocode.

| File | What it shows |
| --- | --- |
| [`mcp_client_config.json`](mcp_client_config.json) | Wiring the server into an MCP client |
| [`product_studio.md`](product_studio.md) | **The demo.** Fourteen calls, empty scene to rendered PNG — a recorded transcript with real timings |
| [`product_shot.jsonc`](product_shot.jsonc) | A product render from an empty scene |
| [`procedural_scatter.jsonc`](procedural_scatter.jsonc) | Geometry nodes, driven from Rust plans |
| [`batch_and_rollback.jsonc`](batch_and_rollback.jsonc) | Typed references, atomic batches, what failure looks like |

To run the server for these:

```bash
cargo build --release              # or download a release binary
python scripts/install_addon.py    # or install the extension zip in Blender
# enable the add-on in Blender and connect from the MCP panel
```

## Reading them

Each file is a sequence of MCP `tools/call` payloads:

```jsonc
{"name": "<tool>", "arguments": { … }}     // → what comes back
```

Responses are abbreviated to the fields that matter. Comments explain the *why*,
which is usually the part that is not obvious from the call.

## Two things to notice

**Nothing here sends code.** Every call is a typed argument structure. The
complicated ones — a twelve-node shader graph, a scatter setup, a lighting rig —
are planned in Rust and applied in a single call, not assembled by sending
Python.

**Failures are structured.** When something goes wrong the error names the field,
gives the value, and lists the alternatives. See the second half of
`batch_and_rollback.jsonc`.

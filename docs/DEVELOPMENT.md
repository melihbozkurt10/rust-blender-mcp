# Development

## Getting set up

You need Rust 1.88+ and Blender 4.2+.

```bash
git clone <this repository>
cd blender-mcp
cargo build --release
python scripts/install_addon.py
```

`install_addon.py` finds Blender, asks it where its extensions directory is, and
links `blender_extension/` into it — so editing the add-on source is immediately
live. Where symlinks are not permitted (Windows without developer mode) it copies
instead and tells you it did.

Then in Blender: **Edit → Preferences → Add-ons**, search for MCP, enable it, and
use the **MCP** panel in the 3D viewport sidebar (press `N`) to connect.

### On Windows

The MSVC toolchain works if you have the Visual Studio build tools. If you do
not, the GNU toolchain plus MinGW is less to install:

```powershell
winget install BrechtSanders.WinLibs.POSIX.UCRT
rustup default stable-x86_64-pc-windows-gnu
```

MinGW's `bin` must be on `PATH`: some crates need `dlltool.exe`, and the error
when it is missing does not say so.

There is deliberately no `rust-toolchain.toml`. Pinning `channel = "stable"`
resolves to the *host* triple, which on Windows means MSVC — so the pin would
break exactly the developers who chose the GNU toolchain because they have no
MSVC linker. `rust-version` in `Cargo.toml` states the real floor (1.88).

## Running it

```bash
# as an MCP server, over stdio
./target/release/blender-mcp

# with everything registered up front
BLENDER_MCP_EAGER_TOOLS=1 ./target/release/blender-mcp

# with logging
RUST_LOG=blender_mcp_server=debug,blender_client=debug ./target/release/blender-mcp
```

Logs go to stderr, because stdout is the MCP transport. Writing anything to
stdout that is not a JSON-RPC message breaks the protocol; if you add a
`println!`, that is the bug.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `BLENDER_MCP_HOST` | `127.0.0.1` | Bridge listen address |
| `BLENDER_MCP_PORT` | `9877` | Bridge listen port |
| `BLENDER_MCP_ALLOW_REMOTE` | unset | Required to bind off-loopback |
| `BLENDER_MCP_WORKSPACE` | platform data dir | Root of the managed tree |
| `BLENDER_MCP_PROJECT_ROOT` | `<workspace>/project` | Where imports and exports resolve |
| `BLENDER_MCP_EAGER_TOOLS` | `0` | Register every category at startup |
| `BLENDER_MCP_CATEGORIES` | `core` | Categories on at startup |
| `BLENDER_MCP_MAX_FRAME_BYTES` | `16777216` | Bridge frame ceiling |
| `BLENDER_MCP_REQUEST_TIMEOUT_SECS` | `15` | Default request deadline |
| `BLENDER_MCP_MAX_BATCH_OPERATIONS` | `200` | Batch size cap |
| `BLENDER_MCP_REVISION_HISTORY` | `1000` | Revisions kept for diffing |
| `BLENDER_MCP_ALLOW_ASSET_DOWNLOADS` | `1` | Allow downloads at all |
| `BLENDER_MCP_MAX_DOWNLOAD_BYTES` | `536870912` | Per-file download cap |
| `BLENDER_MCP_SKETCHFAB_TOKEN` | unset | Sketchfab credentials |

Environment rather than a config file: an MCP client already has somewhere to put
environment variables, and a file would be one more thing to keep in sync.

## Repository layout

```
crates/
  blender-protocol/    the contract: ids, math, errors, envelopes, payloads
  blender-client/      framing, sessions, reconnection, timeouts
  blender-domain/      geometry, framing, lighting, graph plans, validation
  workflow-engine/     multi-step runs with compensating rollback
  scene-cache/         revisions, diffs, invalidation
  asset-providers/     external libraries, download policy, credentials
  mcp-server/          tools, registry, artifacts, config, the binary
blender_extension/     the Blender add-on
  operations/          one module per domain; every handler is decorated
scripts/               install, package, verify, smoke test, headless bridge
tests/
  protocol/            cross-language parity
  blender/             end-to-end over a real socket
docs/
examples/
```

## Adding an operation

Two sides, in this order.

### 1. The payload

In `crates/blender-protocol/src/<domain>.rs`:

```rust
/// `light.update`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateLight {
    pub light: LightRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy: Option<f64>,
}

impl Validate for UpdateLight {
    fn validate(&self) -> Result<()> {
        if let Some(energy) = self.energy {
            check_non_negative(energy, "energy")?;
        }
        Ok(())
    }
}
```

Validate here, not in the handler. A bad value should never reach the wire, and
the error should name the field and say what would have been acceptable.

### 2. The handler

In `blender_extension/operations/<domain>.py`:

```python
@op("light.update")
def update_light(ctx, args: dict) -> dict[str, Any]:
    light = ids.find("light", c.require_str(args, "light"))
    energy = c.optional_float(args, "energy")
    if energy is not None:
        light.data.energy = energy
    ctx.bump()
    return {"light": ids.ensure_id(light), "revision": ctx.revision}
```

`@op` is WRITE, `@read` is READ, `@external` is EXTERNAL_SIDE_EFFECT. The
decorator is the only way to register a handler, and the class must match the
Rust tool's — a test enforces it.

Prefer `bpy.data` over `bpy.ops`. Operators depend on context — the active
object, the area under the cursor, the current mode — which is exactly the thing
that is unpredictable when a call arrives over a socket. Where an operator is
unavoidable, set up the context explicitly and restore it afterwards. Mesh
editing uses `bmesh.ops`, which works headless and needs no context at all.

### 3. The tool

In `crates/mcp-server/src/tools/<domain>.rs`:

```rust
ToolSpec::forward::<UpdateLight>(
    "light.update",
    Category::Lights,
    OpKind::Write,
    "Update a light",
    "Change a light's energy, colour, size or shape. Only the fields you give are touched.",
),
```

The description is read by a model deciding whether to call it. Say what it does
and what the argument means, not what type it is — the schema already says the
type.

Use `ToolSpec::custom` when the server does work of its own: planning, resolving
a path, consulting the cache.

### 4. Prove it

```bash
cargo test --workspace --all-features
cargo test -p blender-mcp-server --test protocol_parity
blender --background --python scripts/smoke_test.py
```

The parity test fails if the tool name and the handler name disagree, or if their
side-effect classes do not match.

## Adding a workflow

1. Plan it in `blender-domain` — the maths, with unit tests against known
   answers, and no I/O.
2. Sequence it in `workflow-engine`, registering a compensation for every step
   that creates something.
3. Expose it in `crates/mcp-server/src/tools/workflows.rs`.
4. Test it against the recording executor, to completion **and** to failure, and
   assert the compensations ran in reverse order.

A workflow that cannot track what it created must fail loudly rather than carry
on: silently skipping a compensation turns "rolled back" into a lie.

## Style

**Rust.** `cargo fmt`, and `cargo clippy --all-targets -- -D warnings` clean.
Comments explain *why*, never *what* — the code says what. A comment that
restates the line above it is noise; a comment recording why the obvious approach
was wrong is worth more than the code it sits on.

**Python.** Standard library only. Type hints on handler signatures. Errors are
`BridgeError` with a code from the taxonomy and details a caller can act on.

**Errors.** Every error names the field, gives the value and lists the
alternatives when there is a list. `"invalid input"` is not an error message.

## Debugging

**Nothing connects.** Check the MCP panel in Blender's sidebar — it shows the
connection state and the last error. Check the port matches on both sides.

**The tool list is empty or stale.** Lazy mode, and the client did not refresh on
`notifications/tools/list_changed`. Restart the server with
`BLENDER_MCP_EAGER_TOOLS=1`.

**An operation fails with `BLENDER_CONTEXT_ERROR`.** An operator was used where
`bpy.data` would have worked, or the context was not set up. Look for
`bpy.ops.*` in the handler.

**Blender's UI freezes.** Something blocked the main thread. The pump must never
block on the network; the reader thread must never touch `bpy`.

**A tool's arguments are not what you expected.** Print the schema the client
actually sees:

```bash
cargo run -p blender-mcp-server --example tool_schema -- camera.auto_frame
cargo run -p blender-mcp-server --example tool_inventory -- --names
```

**A change is invisible to `scene.diff`.** The handler did not call `ctx.bump()`,
or did not emit an event.

## Releasing

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
python scripts/verify_repo.py
blender --background --python scripts/smoke_test.py

cargo build --release
python scripts/package_addon.py
```

`package_addon.py` writes a reproducible zip to `dist/` and prints its SHA-256.
It refuses to build if any file looks like it contains a credential.

Bump `PROTOCOL_VERSION` only when an existing message shape changes meaning. A
new field or a new operation does not need it — that is what capability
negotiation is for.

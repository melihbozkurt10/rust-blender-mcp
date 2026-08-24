# Rust Blender MCP — quickstart

A typed, Rust-first MCP server for Blender with persistent IPC and no arbitrary
Python execution.

This archive holds the server binary. You also need the Blender extension, which
is a separate download on the same release page:
`rust-blender-mcp-<version>-blender-extension.zip`.

Rust is not required. Nothing here is compiled on your machine.

---

## 1. Install the Blender extension

Download `rust-blender-mcp-<version>-blender-extension.zip`, then in Blender:

**Edit → Preferences → Add-ons → ▾ → Install from Disk…**, pick the zip, and
tick **Blender MCP Bridge** to enable it.

Blender 4.2 LTS or newer. Tested against 4.2 and 5.1.

## 2. Point your MCP client at the binary

Put the executable somewhere permanent, then add it to your client's MCP
configuration. Use the absolute path — MCP clients do not search `PATH`.

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

On Windows the command is the full path to `blender-mcp.exe`, with backslashes
escaped (`C:\\Tools\\blender-mcp.exe`) or written as forward slashes.

Restart the client so it picks the server up.

## 3. Connect Blender

Start the server (your MCP client does this for you), then in Blender press
<kbd>N</kbd> in the 3D viewport, open the **MCP** tab, and press **Connect**.

The add-on dials out to `127.0.0.1:9877` and reconnects on its own if the server
restarts. Nothing listens on a public interface.

To check it worked, ask the assistant to call `blender.status`. It reports the
connection, the Blender version and the add-on version — and, when nothing is
connected, exactly what to do about it.

---

## Then what

Ask for something. The assistant starts with a small `core` tool set and turns
on the categories it needs:

> Create a studio product scene: a bevelled crate on a backdrop, three-point
> lighting, and a camera framed on the crate.

A full worked example is in `examples/product_studio.md` in the repository.

## Configuration

Everything is an environment variable, set in the same MCP client config block.

| Variable | Default | What it does |
| --- | --- | --- |
| `BLENDER_MCP_PORT` | `9877` | Port the add-on dials into |
| `BLENDER_MCP_HOST` | `127.0.0.1` | Bind address. Anything non-loopback is refused unless `BLENDER_MCP_ALLOW_REMOTE=1` |
| `BLENDER_MCP_EAGER_TOOLS` | unset | `1` registers every tool at startup instead of by category |
| `BLENDER_MCP_CATEGORIES` | `core` | Categories enabled before the client asks, comma-separated |
| `BLENDER_MCP_WORKSPACE` | per-user data directory | Where renders, exports and downloads land |
| `BLENDER_MCP_LOG` | `info` | Log filter. Diagnostics go to stderr; stdout carries MCP only |

Set `BLENDER_MCP_EAGER_TOOLS=1` if your client does not refresh its tool list on
`notifications/tools/list_changed`. It costs a much larger tool schema up front,
which is the thing lazy loading exists to avoid.

## If something is wrong

**"Blender bridge is not connected."** Blender is not running, the extension is
not enabled, or it is pointed at a different port. `blender.status` returns the
address it is listening on and the steps to fix it.

**The client shows no tools.** Check the client's MCP log. The server writes
diagnostics to stderr — a version banner, the bind address and the registered
tool count — and never to stdout, which carries the protocol.

**macOS refuses to run the binary.** It is unsigned. Remove the quarantine
attribute: `xattr -d com.apple.quarantine ./blender-mcp`.

**Linux: the binary will not start.** The release build targets glibc 2.35 and
newer (Ubuntu 22.04+). On an older distribution, build from source.

---

Full documentation, sources and issue tracker:
<https://github.com/melih-bozkurt/rust-blender-mcp>

Licensed under Apache-2.0. See `LICENSE` and `NOTICE`.

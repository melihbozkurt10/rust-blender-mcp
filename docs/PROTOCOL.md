# The bridge protocol

What travels between the Rust server and the Blender add-on. This is not the MCP
protocol — that is JSON-RPC over stdio, handled by `rmcp` — it is the private
protocol underneath it.

## Transport

A persistent TCP connection on loopback. The **server listens**
(`127.0.0.1:9877` by default) and the **add-on dials in**. That direction is
deliberate: Blender is started by a person, the server is started by an MCP
client, and a user restarting Blender should reconnect to a running server
rather than the other way round.

The server refuses to bind anywhere other than loopback unless
`BLENDER_MCP_ALLOW_REMOTE=1` is set. There is no authentication, because the
socket is not reachable from off the machine; if you change that, you own the
consequences.

## Framing

```
┌────────────────────────┬──────────────────────────────┐
│ length: u32, big-endian│ payload: UTF-8 JSON          │
└────────────────────────┴──────────────────────────────┘
```

- The length does not include itself.
- The default ceiling is 16 MiB (`BLENDER_MCP_MAX_FRAME_BYTES`). A larger frame
  is not read: the connection is closed with `MESSAGE_TOO_LARGE`, because a
  process that has already allocated the buffer has already lost.
- Both sides handle a partial read, several frames in one read, and a frame
  split across many reads. Neither side assumes a message boundary matches a
  socket read.
- Invalid UTF-8 or malformed JSON is a protocol error, not a crash.

## Handshake

The server sends `hello` as soon as the socket opens; the add-on answers
`hello_ack`.

```jsonc
// server → add-on
{
  "type": "hello",
  "protocol_version": 1,
  "client_name": "blender-mcp-server",
  "client_version": "0.1.0",
  "session_id": "0b7f…"          // minted by the server
}

// add-on → server
{
  "type": "hello_ack",
  "protocol_version": 1,
  "session_id": "0b7f…",         // echoed; a mismatch means a stale instance
  "blender_version": {"major": 4, "minor": 2, "patch": 1},
  "python_version": "3.11.7",
  "addon_version": "0.1.0",
  "platform": "windows",
  "background": false,
  "capabilities": { … },
  "revision": 0
}
```

`protocol_version` is independent of the project version. The add-on and the
server ship separately, and someone will run mismatched builds; bumping the
project version must not invalidate a working pair. The protocol version changes
only when an existing message shape changes meaning — a new optional field or a
new operation does not require it, because capability negotiation covers that.

An incompatible version is rejected at the handshake with `PROTOCOL_MISMATCH`,
naming both versions. A Blender older than 4.2 is rejected with
`UNSUPPORTED_BLENDER_VERSION`.

### Capabilities

The `capabilities` object reports what the *running build* has, not what its
version number implies:

```jsonc
{
  "render_engines":   ["BLENDER_EEVEE_NEXT", "CYCLES"],
  "modifiers":        ["SUBSURF", "BEVEL", …],
  "shader_nodes":     ["ShaderNodeBsdfPrincipled", …],   // bl_idnames
  "geometry_nodes":   ["GeometryNodeMeshCube", …],
  "constraints":      ["COPY_LOCATION", …],
  "bone_constraints": ["IK", …],
  "bake_types":       ["COMBINED", "NORMAL", …],
  "image_formats":    ["PNG", "OPEN_EXR", …],
  "import_formats":   ["GLTF", "FBX", "OBJ", "USD", …],
  "export_formats":   ["GLTF", "FBX", "OBJ", "USD", …]
}
```

The server validates every request against this list before sending it. When
something is missing, the error is `CAPABILITY_UNAVAILABLE` with the available
values and, when there is one, a near-miss suggestion:

```jsonc
{
  "code": "CAPABILITY_UNAVAILABLE",
  "message": "This Blender has no render engine called `WORKBENCH`.",
  "details": {
    "requested": "WORKBENCH",
    "available": ["BLENDER_EEVEE", "CYCLES"],
    "did_you_mean": "BLENDER_EEVEE"
  }
}
```

This is how the project survives Blender 5.x without a fork: Blender 5.1 really
did remove Workbench, and nothing needed changing.

## Envelope

Every frame is one JSON object with a `type` discriminator.

### Request

```jsonc
{
  "type": "request",
  "request_id": "3a1c…",
  "op": "object.transform.set",
  "args": { "object": "…", "location": [0, 0, 1] },
  "timeout_ms": 15000            // optional; the server's per-op default otherwise
}
```

### Response

```jsonc
{ "type": "response", "request_id": "3a1c…", "ok": true,  "result": { … } }
{ "type": "response", "request_id": "3a1c…", "ok": false, "error":  { … } }
```

Responses may arrive out of order. Several requests may be in flight. The
`request_id` is the only thing that correlates them, and the `session_id` is what
distinguishes a late reply from a Blender that has since restarted.

### Event

Unsolicited, from the add-on:

```jsonc
{
  "type": "event",
  "session_id": "0b7f…",
  "revision": 42,
  "event": "created",            // the payload is flattened into the frame
  "kind": "object",
  "id": "5c2e…",
  "name": "Cube"
}
```

Events feed the scene cache. The `event` values are `created`, `deleted`,
`renamed`, `modified`, `mesh_invalidated`, `node_tree_invalidated`,
`selection_changed`, `file_reloaded`, `scene_changed`.

Events are deliberately coarse. Serialising every node tweak on every depsgraph
update would cost more than it saves, so fine-grained data — meshes, node trees —
is *invalidated* rather than described, and re-read on demand. The last two are treated as a reset: everything
recorded before them is discarded rather than reported as still true.

## Operation classification

Every operation is `READ`, `WRITE`, or `EXTERNAL_SIDE_EFFECT`, declared on both
sides — in the Rust tool definition and in the add-on's decorator — and a test
fails if the two disagree. The class decides:

| | READ | WRITE | EXTERNAL_SIDE_EFFECT |
| --- | --- | --- | --- |
| Retry after a dropped connection | yes | no | no |
| May join an undo-backed batch | yes | yes | **no** |
| Rolled back automatically | n/a | yes | **never** |

A write is never retried transparently: the bridge may have applied it before
dying. An external side effect — a render written to disk, a file exported, an
asset downloaded — is never claimed to be transactional, because it is not.

## Timeouts

Per-operation, chosen by longest-prefix match on the name, so a render is not
killed at fifteen seconds and a typo does not hang for ten minutes:

| Prefix | Default |
| --- | --- |
| (anything else) | 15 s |
| `mesh.` | 60 s |
| `render.viewport_screenshot` | 60 s |
| `modifier.apply`, `uv.`, `scene.cleanup`, `geometry_nodes.scatter` | 120 s |
| `render.execute`, `io.import`, `io.export`, `rig.auto_weights`, `workflow.`, `batch.execute` | 300 s |
| `texture.bake`, `batch.export`, `asset.download`, `asset.import`, `batch.dispatch` | 600 s |
| `batch.render_cameras`, `batch.turntable`, `workflow.product_turntable` | 900 s |

Longest prefix wins, so `render.viewport_screenshot` gets the screenshot budget
rather than the render one.

A timeout produces `TIMEOUT`, and the request is dropped from the pending map. If
a late response arrives for a dropped request it is discarded with a debug log,
not misattributed to a later one.

## Errors

```jsonc
{
  "code": "INVALID_NODE_SOCKET",
  "message": "`Fac` is not an input on this node.",
  "details": {
    "node": "noise",
    "requested": "Fac",
    "available": ["Vector", "Scale", "Detail", "Roughness", "Distortion"]
  },
  "retryable": false
}
```

The full taxonomy:

**Connection** `BLENDER_NOT_CONNECTED`, `PROTOCOL_MISMATCH`,
`CAPABILITY_UNAVAILABLE`

**Input** `INVALID_ARGUMENT`, `INVALID_ENUM`, `INVALID_TRANSFORM`,
`INVALID_PATH`, `INVALID_NODE_TYPE`, `INVALID_NODE_SOCKET`, `INVALID_PROPERTY`

**Lookup** `OBJECT_NOT_FOUND`, `COLLECTION_NOT_FOUND`, `MATERIAL_NOT_FOUND`,
`NODE_NOT_FOUND`, `NODE_TREE_NOT_FOUND`, `IMAGE_NOT_FOUND`,
`ARMATURE_NOT_FOUND`, `BONE_NOT_FOUND`, `ACTION_NOT_FOUND`, `CAMERA_NOT_FOUND`,
`LIGHT_NOT_FOUND`, `MODIFIER_NOT_FOUND`, `SCENE_NOT_FOUND`,
`ARTIFACT_NOT_FOUND`

**Staleness** `TOPOLOGY_STALE`, `REVISION_EXPIRED`

**Support** `UNSUPPORTED_OPERATION`, `UNSUPPORTED_FORMAT`,
`UNSUPPORTED_PROPERTY`, `UNSUPPORTED_BLENDER_VERSION`

**Blender-side** `BLENDER_CONTEXT_ERROR`, `BLENDER_MODE_ERROR`,
`BLENDER_INTERNAL_ERROR`

**Batching** `TRANSACTION_FAILED`, `TRANSACTION_UNSUPPORTED`, `ROLLBACK_FAILED`

**Transport** `TIMEOUT`, `CONNECTION_LOST`, `MESSAGE_TOO_LARGE`, `RATE_LIMITED`

**Assets** `ASSET_PROVIDER_ERROR`, `ASSET_NOT_FOUND`, `ASSET_DOWNLOAD_FAILED`,
`ASSET_AUTH_REQUIRED`, `ASSET_LICENSE_RESTRICTED`

**Policy** `PATH_NOT_ALLOWED`, `PERMISSION_DENIED`

`retryable` defaults from the code — `BLENDER_NOT_CONNECTED`, `TIMEOUT`,
`CONNECTION_LOST`, `RATE_LIMITED`, `ASSET_PROVIDER_ERROR` and
`ASSET_DOWNLOAD_FAILED` are retryable — and may be overridden per error.

The Rust enum and the add-on's mirror are kept in step by
`tests/protocol/test_error_parity.py`, which fails if either side gains, loses
or renames a code.

## Reconnection

The add-on reconnects with exponential backoff and jitter. On reconnect it gets a
**new session id**, and the server:

- fails every pending request with `CONNECTION_LOST`;
- discards the recorded revision history, because the new session numbers its
  revisions independently;
- re-reads capabilities, since the user may have restarted into a different
  Blender.

Ids survive: they live in the .blend file, not in the connection.

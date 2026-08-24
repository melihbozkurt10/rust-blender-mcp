"""Constants and tunables for the bridge add-on.

Everything here is deliberately boring. The add-on is the thinnest part of the
system: it moves frames, dispatches to a fixed table of handlers, and calls
``bpy``. Policy lives in the Rust server.
"""

from __future__ import annotations

# Wire protocol version. Must match ``blender_protocol::version::PROTOCOL_VERSION``.
PROTOCOL_VERSION = 1

# Add-on version, independent of the protocol version.
ADDON_VERSION = "0.1.0"

# Where the MCP server listens by default.
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 9877

# Largest frame accepted in either direction. Must not exceed the server's
# limit, or a legitimate payload would be written and then rejected.
MAX_FRAME_BYTES = 16 * 1024 * 1024

# Header is a 4-byte big-endian unsigned length.
HEADER_LEN = 4
HEADER_FORMAT = ">I"

# How often the main-thread pump runs, in seconds.
#
# A request cannot be answered before the next pump tick, so this interval *is*
# the round-trip latency of every operation -- measurement puts the rest of the
# bridge (socket, framing, JSON, dispatch) at about 0.15 ms, which is nothing
# beside it. One fixed interval therefore has to choose between a responsive
# bridge and a timer that never wakes for nothing, and there is no reason to
# choose: the pump runs fast while a session is actually driving it and slows
# down when the session goes quiet.
#
# Cadence while requests are in flight, or within PUMP_ACTIVE_WINDOW of the last
# one. 1 kHz, but only for as long as somebody is asking for something.
PUMP_INTERVAL_BUSY = 0.001

# Cadence when nothing has arrived for a while. 125 Hz: each idle tick is one
# non-blocking queue read that finds nothing, so the cost is far below what a
# viewport redraw does, and the first request of a new burst waits at most this
# long before the pump speeds up again.
PUMP_INTERVAL_IDLE = 0.008

# How long after the last handled request the pump stays at the busy cadence.
# Long enough to cover the gap between an assistant's tool calls, short enough
# that an abandoned session settles down on its own.
PUMP_ACTIVE_WINDOW = 0.5

# Upper bound on how long one pump pass may spend running handlers before
# yielding back to Blender. Without this, a queue of slow operations freezes
# the UI for as long as it takes to drain.
PUMP_BUDGET_SECONDS = 0.05

# Socket read timeout, so the reader thread can notice a stop request.
SOCKET_TIMEOUT = 0.5

# Reconnect backoff, in seconds.
RECONNECT_INITIAL = 1.0
RECONNECT_MAX = 15.0
RECONNECT_MULTIPLIER = 2.0

# Bounded queues. If the server floods the bridge faster than Blender can
# execute, the reader blocks, which applies backpressure through TCP rather
# than growing a queue until memory runs out.
INBOX_MAX = 256
OUTBOX_MAX = 256

# Custom property that carries the stable entity id.
ID_PROPERTY = "mcp_id"

# Custom property recording the mesh topology revision.
MESH_REVISION_PROPERTY = "mcp_mesh_revision"

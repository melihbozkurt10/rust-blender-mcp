"""The benchmark suites.

Each suite is a function that takes a :class:`Context` and returns a JSON-ready
dict. They are deliberately independent: one that cannot run (no Blender, no
packaged add-on) is skipped with a reason rather than failing the run.

What is measured, and what each number does *not* include, is written next to
the measurement rather than left for the reader to guess. The distinction that
matters most: `mcp_roundtrip` never touches Blender, `bridge_floor` never
touches the Rust server, and `blender_ops` is the whole stack. Nothing is
derived by subtracting one from another.
"""

from __future__ import annotations

import json
import math
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from typing import Any, Callable

from . import harness
from .harness import (
    Blender,
    McpClient,
    REPO_ROOT,
    Stats,
    Timer,
    count_tokens,
    directory_size,
    free_port,
    start_stack,
)


@dataclass
class Context:
    binary: str
    blender_exe: str | None
    scale: float = 1.0
    quick: bool = False

    def sized(self, full: int, minimum: int = 1) -> int:
        """Scale a sample count, never below ``minimum``."""
        return max(minimum, int(round(full * self.scale)))


Suite = Callable[[Context], dict[str, Any]]


def _skip(reason: str) -> dict[str, Any]:
    return {"skipped": True, "reason": reason}


# --- A: MCP core round trip -------------------------------------------------


def mcp_roundtrip(context: Context) -> dict[str, Any]:
    """Client -> stdio -> server -> registry -> validation -> handler -> back.

    `blender.status` is the right probe: it is a real registered tool with a
    real schema and a real handler, and its handler answers from server state
    without crossing the bridge. So this is the cost of the MCP layer itself,
    with Blender's contribution held at zero rather than estimated away.

    The number includes the client's own `json.dumps`/`json.loads` in CPython,
    because a client always pays that and pretending otherwise would flatter
    the result.
    """
    warm = context.sized(1_000, 100)
    total = context.sized(10_000, 500)

    stack = start_stack(context.binary, None, with_blender=False)
    try:
        client = stack.client
        for _ in range(warm):
            client.call_tool("blender.status")

        samples: list[float] = []
        started = Timer()
        for _ in range(total):
            at = Timer()
            client.call_tool("blender.status")
            samples.append(Timer() - at)
        wall = Timer() - started

        # How much of the above is the harness rather than the server. Measured,
        # not assumed: the same encode/decode work against an in-process buffer.
        payload = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "blender.status", "arguments": {}}}
        loop = context.sized(10_000, 500)
        at = Timer()
        for _ in range(loop):
            json.loads(json.dumps(payload))
        client_json_ms = (Timer() - at) / loop * 1e3

        return {
            "tool": "blender.status",
            "path": "MCP client -> stdio -> Rust server -> handler -> back (no Blender IPC)",
            "warmup_requests": warm,
            "stats": Stats.of(samples, wall).as_dict(),
            "client_json_overhead_ms": round(client_json_ms, 4),
        }
    finally:
        stack.close()


# --- B: startup -------------------------------------------------------------


def startup(context: Context) -> dict[str, Any]:
    """Cold start, in the four stages a user actually waits through."""
    samples = context.sized(7, 3)
    server_only: list[float] = []
    blender_connect: list[float] = []
    ready_to_operate: list[float] = []
    first_op: list[float] = []

    for _ in range(samples):
        if context.blender_exe is None:
            stack = start_stack(context.binary, None, with_blender=False)
            try:
                server_only.append(stack.startup["server_ready_s"])
            finally:
                stack.close()
            continue
        stack = start_stack(context.binary, context.blender_exe, with_blender=True)
        try:
            server_only.append(stack.startup["server_ready_s"])
            blender_connect.append(stack.startup["blender_connect_s"])
            ready_to_operate.append(stack.startup["ready_to_operate_s"])
            at = Timer()
            stack.client.call_tool("blender.capabilities")
            first_op.append(Timer() - at)
        finally:
            stack.close()

    result: dict[str, Any] = {
        "samples": samples,
        "note": (
            "Server-ready is spawn to a completed MCP `initialize`. Blender-connect "
            "is a cold `blender --background --factory-startup` launching the bridge "
            "and completing the socket handshake; most of it is Blender's own start-up, "
            "not the bridge's."
        ),
        "server_ready": Stats.of(server_only).as_dict(),
    }
    if blender_connect:
        result["blender_connect"] = Stats.of(blender_connect).as_dict()
        result["ready_to_operate"] = Stats.of(ready_to_operate).as_dict()
        result["first_capabilities_call"] = Stats.of(first_op).as_dict()
    else:
        result["blender"] = "not measured (no Blender found)"
    return result


# --- bridge floor -----------------------------------------------------------


class RawBridge:
    """Speaks the bridge wire protocol directly, with no Rust server involved.

    This exists only to measure the IPC floor: framing, the socket, the
    main-thread pump interval and the dispatcher, with a handler that does no
    `bpy` work at all. It is a benchmark harness, not a second implementation --
    nothing in the product imports it.
    """

    def __init__(self, port: int) -> None:
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", port))
        self.listener.listen(1)
        self.connection: socket.socket | None = None
        self.session_id = str(uuid.uuid4())
        self.identity: dict[str, Any] = {}

    def accept(self, timeout: float = 180.0) -> None:
        self.listener.settimeout(timeout)
        self.connection, _ = self.listener.accept()
        self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.connection.settimeout(timeout)
        self._write({
            "type": "hello",
            "protocol_version": 1,
            "client_name": "blender-mcp-bench",
            "client_version": "0.1.0",
            "session_id": self.session_id,
        })
        ack = self._read()
        if ack.get("type") != "hello_ack":
            raise RuntimeError(f"expected hello_ack, got {ack.get('type')}")
        self.identity = ack

    def _write(self, frame: dict) -> None:
        assert self.connection is not None
        payload = json.dumps(frame).encode("utf-8")
        self.connection.sendall(struct.pack(">I", len(payload)) + payload)

    def _read_exactly(self, count: int) -> bytes:
        assert self.connection is not None
        chunks = []
        remaining = count
        while remaining:
            chunk = self.connection.recv(remaining)
            if not chunk:
                raise RuntimeError("bridge closed the connection")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def _read(self) -> dict:
        size = struct.unpack(">I", self._read_exactly(4))[0]
        return json.loads(self._read_exactly(size).decode("utf-8")) if size else {}

    def call(self, op: str, args: dict | None = None) -> dict:
        request_id = str(uuid.uuid4())
        self._write({
            "type": "request",
            "request_id": request_id,
            "command": {"op": op, "args": args or {}},
        })
        while True:
            frame = self._read()
            # Events share the socket and must not be mistaken for the answer.
            if frame.get("type") == "response" and frame.get("request_id") == request_id:
                return frame

    def close(self) -> None:
        try:
            if self.connection:
                self.connection.close()
        finally:
            self.listener.close()


def bridge_floor(context: Context) -> dict[str, Any]:
    """IPC round trip with a handler that does no Blender work.

    `system.ping` is a registered bridge operation that returns a constant. It
    is not an MCP tool and is not exposed to a model; it exists for exactly this
    kind of liveness check. What is left in the number is: socket, framing, JSON,
    the inbox queue, the wait for the next main-thread pump, and dispatch.
    """
    if context.blender_exe is None:
        return _skip("no Blender executable found")

    warm = context.sized(200, 50)
    total = context.sized(2_000, 200)

    port = free_port()
    workspace = tempfile.mkdtemp(prefix="blender-mcp-floor-")
    bridge = RawBridge(port)
    blender = Blender(context.blender_exe, port, workspace)
    try:
        bridge.accept()
        for _ in range(warm):
            bridge.call("system.ping")

        samples: list[float] = []
        started = Timer()
        for _ in range(total):
            at = Timer()
            bridge.call("system.ping")
            samples.append(Timer() - at)
        wall = Timer() - started

        return {
            "op": "system.ping",
            "path": "benchmark harness -> framed socket -> inbox -> main-thread pump -> dispatch -> back",
            "excludes": "the Rust MCP server and any bpy work",
            "pump_interval_ms": _pump_intervals(),
            "stats": Stats.of(samples, wall).as_dict(),
        }
    finally:
        bridge.close()
        blender.close()
        import shutil

        shutil.rmtree(workspace, ignore_errors=True)


def _pump_intervals() -> dict[str, float]:
    """The add-on's pump cadences, read from its own config rather than repeated."""
    try:
        import importlib.util

        spec = importlib.util.spec_from_file_location(
            "_bench_bridge_config",
            os.path.join(REPO_ROOT, "blender_extension", "config.py"),
        )
        module = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
        spec.loader.exec_module(module)  # type: ignore[union-attr]
        return {
            "busy_ms": float(module.PUMP_INTERVAL_BUSY) * 1e3,
            "idle_ms": float(module.PUMP_INTERVAL_IDLE) * 1e3,
            "active_window_ms": float(module.PUMP_ACTIVE_WINDOW) * 1e3,
        }
    except Exception as error:  # noqa: BLE001
        return {"error": str(error)}  # type: ignore[dict-item]


# --- C: real Blender operations ---------------------------------------------

#: Categories the operation benchmarks need. Enabled up front so activation
#: cost is not folded into an operation's latency.
OPERATION_CATEGORIES = ["scene", "mesh", "modifiers", "materials", "utilities", "lights"]


def _reset_scene(client: McpClient) -> None:
    """Delete every object, so each group starts from the same place."""
    listed = client.call_structured("object.list", {"limit": 1000})
    names = [item.get("id") or item.get("name") for item in listed.get("objects", [])]
    if names:
        client.call_tool("object.delete", {"objects": names}, timeout=300)


def blender_ops(context: Context) -> dict[str, Any]:
    """Per-operation latency for the whole stack, one operation at a time."""
    if context.blender_exe is None:
        return _skip("no Blender executable found")

    reps = context.sized(200, 25)
    stack = start_stack(
        context.binary, context.blender_exe, categories=OPERATION_CATEGORIES
    )
    client = stack.client
    results: dict[str, Any] = {}
    try:
        _reset_scene(client)

        # A subject that every later group can operate on.
        created = client.call_structured(
            "object.create", {"type": "CUBE", "name": "BenchSubject"}
        )
        subject = created["object"]["id"]

        def measure(label: str, action: Callable[[int], None], count: int) -> None:
            for index in range(min(5, count)):
                action(index)
            samples: list[float] = []
            started = Timer()
            for index in range(count):
                at = Timer()
                action(index)
                samples.append(Timer() - at)
            results[label] = Stats.of(samples, Timer() - started).as_dict()

        measure(
            "scene.statistics",
            lambda _i: client.call_tool("scene.statistics"),
            reps,
        )
        measure(
            "object.transform",
            lambda i: client.call_tool(
                "object.transform",
                {"object": subject, "location": {"x": i * 0.001, "y": 0.0, "z": 0.0}},
            ),
            reps,
        )

        # Creation and modifier work mutate the scene, so they run in their own
        # group against a scene that is reset first and cleaned after.
        _reset_scene(client)
        creation = context.sized(100, 10)
        created_ids: list[str] = []

        def create(index: int) -> None:
            result = client.call_structured(
                "object.create", {"type": "CUBE", "name": f"BenchCube{index}"}
            )
            created_ids.append(result["object"]["id"])

        measure("object.create", create, creation)

        modifier_reps = context.sized(100, 10)
        targets = created_ids[-modifier_reps:] if created_ids else []
        if len(targets) >= modifier_reps:
            measure(
                "modifier.add",
                lambda i: client.call_tool(
                    "modifier.add", {"object": targets[i], "type": "BEVEL"}
                ),
                modifier_reps,
            )

        client.call_tool("material.create", {"name": "BenchMaterial"})
        assign_reps = context.sized(100, 10)
        assign_targets = created_ids[:assign_reps]
        if len(assign_targets) >= assign_reps:
            measure(
                "material.assign",
                lambda i: client.call_tool(
                    "material.assign",
                    {"material": "BenchMaterial", "objects": [assign_targets[i]]},
                ),
                assign_reps,
            )

        _reset_scene(client)

        return {
            "path": "MCP client -> Rust server -> framed IPC -> Blender main thread -> bpy -> back",
            "note": (
                "Each figure is one whole round trip. It is not split into "
                "server / IPC / bpy components because the bridge does not "
                "timestamp the stages, and a split that is not measured would "
                "be a guess. Compare against `bridge_floor`, which is the same "
                "path with the bpy work removed."
            ),
            "operations": results,
        }
    finally:
        stack.close()


# --- D: sequential ----------------------------------------------------------


def sequential(context: Context) -> dict[str, Any]:
    """N individual transforms, each its own MCP call."""
    if context.blender_exe is None:
        return _skip("no Blender executable found")

    counts = [100, 500, 1000] if not context.quick else [100]
    counts = [max(10, int(round(n * context.scale))) for n in counts]

    stack = start_stack(
        context.binary, context.blender_exe, categories=OPERATION_CATEGORIES
    )
    client = stack.client
    runs: dict[str, Any] = {}
    try:
        for count in counts:
            _reset_scene(client)
            created = client.call_structured(
                "object.create", {"type": "CUBE", "name": "SeqSubject"}
            )
            subject = created["object"]["id"]
            for index in range(5):
                client.call_tool(
                    "object.transform",
                    {"object": subject, "location": {"x": 0.0, "y": 0.0, "z": 0.0}},
                )

            samples: list[float] = []
            started = Timer()
            for index in range(count):
                at = Timer()
                client.call_tool(
                    "object.transform",
                    {
                        "object": subject,
                        "location": {"x": index * 0.001, "y": 0.0, "z": 0.0},
                    },
                )
                samples.append(Timer() - at)
            wall = Timer() - started
            runs[str(count)] = {
                "operations": count,
                "total_s": round(wall, 4),
                "ops_per_second": round(count / wall, 1),
                "stats": Stats.of(samples, wall).as_dict(),
            }
        _reset_scene(client)
        return {
            "operation": "object.transform",
            "note": "One MCP tool call per transform: N request/response round trips.",
            "runs": runs,
        }
    finally:
        stack.close()


# --- E: batch ---------------------------------------------------------------


def batch(context: Context) -> dict[str, Any]:
    """The same transforms through `batch.execute`, against the individual path."""
    if context.blender_exe is None:
        return _skip("no Blender executable found")

    stack = start_stack(
        context.binary, context.blender_exe, categories=OPERATION_CATEGORIES
    )
    client = stack.client

    limit = 200  # server default for BLENDER_MCP_MAX_BATCH_OPERATIONS
    sizes = [10, 100, 500, 1000] if not context.quick else [10, 100]
    sizes = [max(2, int(round(n * context.scale))) for n in sizes]

    runs: dict[str, Any] = {}
    try:
        for size in sizes:
            _reset_scene(client)
            created = client.call_structured(
                "object.create", {"type": "CUBE", "name": "BatchSubject"}
            )
            subject = created["object"]["id"]

            operations = [
                {
                    "op": "object.transform",
                    "args": {
                        "object": subject,
                        "location": {"x": i * 0.001, "y": 0.0, "z": 0.0},
                    },
                }
                for i in range(size)
            ]

            # Individual baseline for exactly this size.
            for _ in range(5):
                client.call_tool(
                    "object.transform",
                    {"object": subject, "location": {"x": 0.0, "y": 0.0, "z": 0.0}},
                )
            at = Timer()
            for operation in operations:
                client.call_tool("object.transform", operation["args"])
            individual_s = Timer() - at

            entry: dict[str, Any] = {
                "operations": size,
                "individual_total_s": round(individual_s, 4),
                "individual_per_op_ms": round(individual_s / size * 1e3, 4),
            }

            if size > limit:
                # A batch larger than the server's cap is refused, by design.
                # Chunk it, which is what a caller would actually do, and say so.
                chunks = [
                    operations[i : i + limit] for i in range(0, len(operations), limit)
                ]
                at = Timer()
                for chunk in chunks:
                    client.call_tool(
                        "batch.execute", {"operations": chunk, "mode": "STOP_ON_ERROR"},
                        timeout=600,
                    )
                batch_s = Timer() - at
                entry["batch_chunks"] = len(chunks)
                entry["batch_chunk_size"] = limit
                entry["note"] = (
                    f"{size} exceeds the {limit}-operation batch cap, so this is "
                    f"{len(chunks)} chunked batches, not one."
                )
            else:
                at = Timer()
                client.call_tool(
                    "batch.execute",
                    {"operations": operations, "mode": "STOP_ON_ERROR"},
                    timeout=600,
                )
                batch_s = Timer() - at
                entry["batch_chunks"] = 1

            entry["batch_total_s"] = round(batch_s, 4)
            entry["batch_per_op_ms"] = round(batch_s / size * 1e3, 4)
            entry["speedup"] = round(individual_s / batch_s, 2) if batch_s > 0 else None
            runs[str(size)] = entry

        _reset_scene(client)
        return {
            "operation": "object.transform",
            "batch_cap": limit,
            "note": (
                "`speedup` above 1 means the batch was faster. Batching removes "
                "per-call round trips; it does not make the underlying bpy call "
                "any faster, so the ceiling is set by how much of a call's cost "
                "was the round trip."
            ),
            "runs": runs,
        }
    finally:
        stack.close()


# --- F: tool schema / context footprint -------------------------------------

#: The combinations worth reporting. `modelling_session` is what a real
#: modelling conversation ends up with, not a synthetic worst case.
FOOTPRINT_SETS: list[tuple[str, list[str] | None]] = [
    ("core", []),
    ("core+scene", ["scene"]),
    ("core+mesh", ["mesh"]),
    ("core+materials", ["materials"]),
    ("modelling_session", ["scene", "mesh", "modifiers", "materials"]),
    ("all", None),
]


def context_footprint(context: Context) -> dict[str, Any]:
    """What each category combination costs in tool-list bytes and tokens."""
    entries: dict[str, Any] = {}
    tokenizer = "estimate"
    for label, categories in FOOTPRINT_SETS:
        eager = categories is None
        stack = start_stack(
            context.binary,
            None,
            categories=categories or [],
            eager=eager,
            with_blender=False,
        )
        try:
            client = stack.client
            # Warm, then measure: the first list pays for whatever the SDK
            # lazily initialises, which is not what a session's steady state
            # costs.
            client.list_tools()
            samples = []
            for _ in range(context.sized(50, 10)):
                at = Timer()
                tools = client.list_tools()
                samples.append(Timer() - at)
            payload = json.dumps(tools, separators=(",", ":"), ensure_ascii=False)
            tokens, tokenizer = count_tokens(payload)
            entries[label] = {
                "categories": "all" if eager else ["core"] + (categories or []),
                "tool_count": len(tools),
                "schema_bytes": len(payload.encode("utf-8")),
                "schema_kb": round(len(payload.encode("utf-8")) / 1024, 1),
                "tokens": tokens,
                "tools_list_ms": Stats.of(samples).as_dict(),
            }
        finally:
            stack.close()

    # Turning a category on mid-session is the flow the design depends on, so
    # its cost belongs in the same table.
    stack = start_stack(context.binary, None, with_blender=False)
    try:
        client = stack.client
        client.list_tools()
        at = Timer()
        client.call_tool("tools.categories.enable", {"category": "mesh"})
        enable_ms = (Timer() - at) * 1e3
        at = Timer()
        after = client.list_tools()
        relist_ms = (Timer() - at) * 1e3
        names_after = {tool["name"] for tool in after}
        client.call_tool("tools.categories.disable", {"category": "mesh"})
        names_back = {tool["name"] for tool in client.list_tools()}
        activation = {
            "enable_ms": round(enable_ms, 3),
            "relist_ms": round(relist_ms, 3),
            "tools_after_enable": len(names_after),
            "tools_after_disable": len(names_back),
            "disable_leaves_no_stale_tools": bool(names_back < names_after),
        }
    finally:
        stack.close()

    return {
        "tokenizer": tokenizer,
        "tokenizer_note": (
            "`estimate` is the documented deterministic estimator in "
            "benchmarks/harness.py, not a model tokenizer. It is an estimate; "
            "byte counts are exact."
        ),
        "measured": "the exact JSON of an MCP `tools/list` reply, as a client receives it",
        "sets": entries,
        "activation": activation,
    }


# --- G: memory --------------------------------------------------------------


def memory(context: Context) -> dict[str, Any]:
    """Server process memory at the points a user would notice it."""
    try:
        import psutil
    except ImportError:
        return _skip("psutil is not installed (pip install psutil)")

    def rss_mb(process: "psutil.Process") -> float:
        info = process.memory_info()
        # `rss` on Windows is the working set, which is the number Task Manager
        # shows and the one a user will compare against.
        return round(info.rss / (1024 * 1024), 2)

    stack = start_stack(
        context.binary,
        context.blender_exe,
        categories=OPERATION_CATEGORIES,
        with_blender=context.blender_exe is not None,
    )
    try:
        process = psutil.Process(stack.client.pid)
        points: dict[str, Any] = {"startup_rss_mb": rss_mb(process)}

        stack.client.list_tools()
        points["idle_rss_mb"] = rss_mb(process)

        requests = context.sized(1_000, 200)
        for _ in range(requests):
            stack.client.call_tool("blender.status")
        points[f"after_{requests}_requests_rss_mb"] = rss_mb(process)

        if stack.blender is not None:
            _reset_scene(stack.client)
            created = stack.client.call_structured(
                "object.create", {"type": "CUBE", "name": "MemSubject"}
            )
            subject = created["object"]["id"]
            size = context.sized(200, 20)
            stack.client.call_tool(
                "batch.execute",
                {
                    "operations": [
                        {
                            "op": "object.transform",
                            "args": {
                                "object": subject,
                                "location": {"x": i * 0.001, "y": 0.0, "z": 0.0},
                            },
                        }
                        for i in range(min(size, 200))
                    ],
                    "mode": "STOP_ON_ERROR",
                },
                timeout=600,
            )
            points["after_batch_rss_mb"] = rss_mb(process)
            _reset_scene(stack.client)

        points["platform"] = sys.platform
        points["metric"] = (
            "resident set size; on Windows this is the process working set"
        )
        return points
    finally:
        stack.close()


# --- H: distribution size ---------------------------------------------------


def distribution(context: Context) -> dict[str, Any]:
    """What a user downloads, and what they emphatically do not."""
    result: dict[str, Any] = {}

    binary_bytes = os.path.getsize(context.binary)
    result["binary"] = {
        "path": os.path.relpath(context.binary, REPO_ROOT).replace("\\", "/"),
        "bytes": binary_bytes,
        "human": harness.human_bytes(binary_bytes),
    }

    out_dir = os.path.join(tempfile.gettempdir(), "blender-mcp-bench-dist")
    try:
        completed = subprocess.run(
            [sys.executable, os.path.join(REPO_ROOT, "scripts", "package_addon.py"),
             "--out", out_dir],
            cwd=REPO_ROOT, capture_output=True, text=True, timeout=300,
        )
        if completed.returncode != 0:
            result["extension"] = {"error": completed.stderr.strip()[:400]}
        else:
            zips = [
                os.path.join(out_dir, name)
                for name in os.listdir(out_dir)
                if name.endswith(".zip")
            ]
            newest = max(zips, key=os.path.getmtime)
            size = os.path.getsize(newest)
            result["extension"] = {
                "file": os.path.basename(newest),
                "bytes": size,
                "human": harness.human_bytes(size),
            }
    except Exception as error:  # noqa: BLE001
        result["extension"] = {"error": str(error)[:400]}

    extension_bytes = result.get("extension", {}).get("bytes", 0)
    combined = binary_bytes + extension_bytes
    result["combined_download"] = {
        "bytes": combined,
        "human": harness.human_bytes(combined),
        "note": (
            "Uncompressed. A release ships the binary inside a zip or tar.gz, "
            "which is roughly a third of this -- see the release page for what "
            "is actually downloaded."
        ),
    }

    source_bytes = directory_size(REPO_ROOT, exclude={".git", "target", "dist", "__pycache__"})
    result["source_tree"] = {
        "bytes": source_bytes,
        "human": harness.human_bytes(source_bytes),
        "excludes": [".git", "target", "dist", "__pycache__"],
    }

    target_dir = os.path.join(REPO_ROOT, "target")
    if os.path.isdir(target_dir):
        target_bytes = directory_size(target_dir)
        result["build_cache"] = {
            "path": "target/",
            "bytes": target_bytes,
            "human": harness.human_bytes(target_bytes),
            "note": (
                "A developer's Rust build cache. It is git-ignored, it is never "
                "published, and it has nothing to do with what a user downloads."
            ),
        }
    return result


# --- registry ---------------------------------------------------------------

SUITES: dict[str, Suite] = {
    "mcp_roundtrip": mcp_roundtrip,
    "startup": startup,
    "bridge_floor": bridge_floor,
    "blender_ops": blender_ops,
    "sequential": sequential,
    "batch": batch,
    "context_footprint": context_footprint,
    "memory": memory,
    "distribution": distribution,
}

#: Suites that do not need Blender, for a fast check.
SERVER_ONLY = ["mcp_roundtrip", "context_footprint", "distribution"]

"""End-to-end test: MCP client -> Rust server -> Blender -> back.

Runs the whole stack for real. There is no mocking anywhere: a genuine Blender
process connects to a genuine `blender-mcp` binary, and this script speaks MCP
over the server stdio transport exactly as an MCP client would.

    python tests/blender/test_bridge_roundtrip.py [--blender PATH] [--binary PATH]

Exits non-zero on failure, so it is usable from CI.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

PASSED = 0
FAILED: list[tuple[str, str]] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    global PASSED
    if condition:
        PASSED += 1
        print(f"  ok   {name}")
    else:
        FAILED.append((name, detail))
        print(f"  FAIL {name}: {detail}")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def find_blender(explicit: str | None) -> str:
    if explicit:
        return explicit
    env = os.environ.get("BLENDER_EXECUTABLE")
    if env:
        return env
    found = shutil.which("blender")
    if found:
        return found
    for candidate in [
        r"C:\Program Files\Blender Foundation\Blender 5.1\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 4.5\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 4.2\blender.exe",
        "/usr/bin/blender",
        "/Applications/Blender.app/Contents/MacOS/Blender",
    ]:
        if os.path.exists(candidate):
            return candidate
    raise SystemExit(
        "Blender not found. Pass --blender PATH or set BLENDER_EXECUTABLE."
    )


def find_binary(explicit: str | None) -> str:
    if explicit:
        return explicit
    suffix = ".exe" if os.name == "nt" else ""
    for profile in ("debug", "release"):
        candidate = os.path.join(REPO_ROOT, "target", profile, f"blender-mcp{suffix}")
        if os.path.exists(candidate):
            return candidate
    raise SystemExit("blender-mcp binary not found. Run `cargo build` first.")


class McpClient:
    """A minimal MCP client over the server stdio transport."""

    def __init__(self, command: list[str], env: dict[str, str]) -> None:
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            bufsize=1,
        )
        self._next_id = 0
        self._stderr: list[str] = []
        self._drain = threading.Thread(target=self._drain_stderr, daemon=True)
        self._drain.start()

    def _drain_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self._stderr.append(line.rstrip())

    def stderr_tail(self, count: int = 20) -> str:
        return "\n".join(self._stderr[-count:])

    def request(self, method: str, params: Any = None, timeout: float = 60.0) -> dict:
        self._next_id += 1
        message = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            message["params"] = params
        self._send(message)
        return self._read_reply(self._next_id, timeout)

    def notify(self, method: str, params: Any = None) -> None:
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self._send(message)

    def _send(self, message: dict) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def _read_reply(self, request_id: int, timeout: float) -> dict:
        assert self.process.stdout is not None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError(
                    f"server closed stdout.\nstderr:\n{self.stderr_tail()}"
                )
            line = line.strip()
            if not line:
                continue
            message = json.loads(line)
            # Notifications and other traffic are not what we are waiting for.
            if message.get("id") == request_id:
                return message
        raise TimeoutError(f"no reply to request {request_id} within {timeout}s")

    def call_tool(self, name: str, arguments: dict | None = None, timeout: float = 60.0) -> dict:
        reply = self.request(
            "tools/call",
            {"name": name, "arguments": arguments or {}},
            timeout=timeout,
        )
        if "error" in reply:
            raise RuntimeError(f"{name} failed at the protocol level: {reply['error']}")
        return reply["result"]

    def close(self) -> None:
        try:
            if self.process.stdin:
                self.process.stdin.close()
            self.process.wait(timeout=5)
        except Exception:
            self.process.kill()


def structured(result: dict) -> Any:
    """The structured payload of a tool result."""
    if "structuredContent" in result:
        return result["structuredContent"]
    content = result.get("content") or []
    if content and content[0].get("type") == "text":
        return json.loads(content[0]["text"])
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--blender", default=None)
    parser.add_argument("--binary", default=None)
    parser.add_argument("--keep-going", action="store_true")
    args = parser.parse_args()

    blender = find_blender(args.blender)
    binary = find_binary(args.binary)
    port = free_port()
    workspace = tempfile.mkdtemp(prefix="blender-mcp-e2e-")

    print(f"blender: {blender}")
    print(f"server:  {binary}")
    print(f"port:    {port}")
    print(f"workspace: {workspace}")

    env = dict(os.environ)
    env.update(
        {
            "BLENDER_MCP_PORT": str(port),
            "BLENDER_MCP_WORKSPACE": workspace,
            "BLENDER_MCP_LOG": "warn",
        }
    )

    client = McpClient([binary], env)
    blender_process = None
    try:
        # -- MCP handshake --------------------------------------------------
        init = client.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "blender-mcp-e2e", "version": "0.1.0"},
            },
        )
        check("initialize", "result" in init, json.dumps(init)[:400])
        server_info = init.get("result", {}).get("serverInfo", {})
        check("serverInfo.name", server_info.get("name") == "blender-mcp", str(server_info))
        instructions = init.get("result", {}).get("instructions", "")
        check(
            "instructions mention no code execution",
            "no tool that runs Python" in instructions or "runs Python" in instructions,
            instructions[:200],
        )
        client.notify("notifications/initialized")

        # -- lazy tool listing ----------------------------------------------
        listed = client.request("tools/list", {})
        core_tools = [t["name"] for t in listed["result"]["tools"]]
        check("core tools listed", len(core_tools) > 0, str(core_tools))
        check(
            "lazy mode hides non-core tools",
            "object.create" not in core_tools,
            f"unexpectedly visible: {core_tools}",
        )
        check("blender.status is core", "blender.status" in core_tools, str(core_tools))
        # The invariant is about *what* a tool does, not about the letters in
        # its name: `batch.execute` runs other tools, `execute_python` would run
        # code. Check name segments against the forbidden words.
        forbidden = {
            "python", "shell", "exec", "eval", "script", "subprocess", "command", "system",
        }
        offenders = [
            name
            for name in core_tools
            for segment in name.replace(".", "_").split("_")
            if segment in forbidden
        ]
        check("no code execution tool is exposed", not offenders, str(offenders))

        # -- status before Blender connects ---------------------------------
        status = structured(client.call_tool("blender.status"))
        check("status reports disconnected", status["connected"] is False, str(status))
        check("status explains how to connect", "how_to_connect" in status, str(status))

        # -- start Blender ---------------------------------------------------
        ready_file = os.path.join(workspace, "ready")
        blender_process = subprocess.Popen(
            [
                blender,
                "--background",
                "--factory-startup",
                "--python",
                os.path.join(REPO_ROOT, "scripts", "run_bridge.py"),
                "--",
                "--port",
                str(port),
                "--seconds",
                "180",
                "--ready-file",
                ready_file,
            ],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not os.path.exists(ready_file):
            if blender_process.poll() is not None:
                output = blender_process.stdout.read() if blender_process.stdout else ""
                raise RuntimeError(f"Blender exited early:\n{output[-3000:]}")
            time.sleep(0.2)
        check("Blender connected", os.path.exists(ready_file), "timed out waiting for the bridge")

        status = structured(client.call_tool("blender.status"))
        check("status reports connected", status["connected"] is True, str(status))
        check("status reports a version", bool(status.get("blender_version")), str(status))
        check("status reports background mode", status.get("background") is True, str(status))

        capabilities = structured(client.call_tool("blender.capabilities"))
        check(
            "capabilities list render engines",
            len(capabilities.get("render_engines", [])) > 0,
            str(capabilities)[:300],
        )

        # -- enabling a category changes the tool list ----------------------
        categories = structured(client.call_tool("tools.categories.list"))
        check("categories listed", len(categories["categories"]) > 1, str(categories)[:200])
        scene_category = next(c for c in categories["categories"] if c["id"] == "scene")
        check("scene starts disabled", scene_category["enabled"] is False, str(scene_category))

        enabled = structured(client.call_tool("tools.categories.enable", {"category": "scene"}))
        check("enable reports the change", enabled["changed"] is True, str(enabled))

        listed = client.request("tools/list", {})
        after = [t["name"] for t in listed["result"]["tools"]]
        check("object.create is now visible", "object.create" in after, str(after)[:300])
        check("tool list grew", len(after) > len(core_tools))

        # -- real work -------------------------------------------------------
        summary = structured(client.call_tool("scene.summary"))
        check("scene.summary works", "objects" in summary, str(summary)[:200])
        baseline = summary["objects"]["total"]

        created = structured(
            client.call_tool(
                "object.create",
                {
                    "type": "CUBE",
                    "name": "E2ECube",
                    "location": {"x": 1.0, "y": 2.0, "z": 3.0},
                    "dimensions": {"x": 4.0, "y": 0.5, "z": 2.5},
                },
            )
        )
        check("object.create succeeded", "object" in created, str(created)[:300])
        cube_id = created["object"]["id"]
        check("id is a uuid", len(cube_id) == 36, cube_id)
        check(
            "dimensions applied",
            abs(created["object"]["dimensions"]["x"] - 4.0) < 1e-4,
            str(created["object"]["dimensions"]),
        )

        summary = structured(client.call_tool("scene.summary"))
        check("object count grew", summary["objects"]["total"] == baseline + 1, str(summary["objects"]))

        # Rename, then prove the id still resolves.
        client.call_tool("object.rename", {"object": cube_id, "name": "E2ERenamed"})
        fetched = structured(client.call_tool("object.get", {"object": cube_id}))
        check("id survives rename", fetched["object"]["name"] == "E2ERenamed", str(fetched)[:200])

        # -- validation happens in Rust, before Blender ----------------------
        bad = client.call_tool(
            "object.transform",
            {"object": cube_id, "scale": {"x": 1.0, "y": 0.0, "z": 1.0}},
        )
        payload = structured(bad)
        check(
            "zero scale rejected with a typed code",
            bad.get("isError") is True and payload["error"]["code"] == "INVALID_TRANSFORM",
            str(payload),
        )

        bad = client.call_tool("object.create", {"type": "DODECAHEDRON"})
        payload = structured(bad)
        check(
            "unknown primitive rejected before Blender",
            bad.get("isError") is True and payload["error"]["code"] == "INVALID_ARGUMENT",
            str(payload),
        )

        missing = client.call_tool("object.get", {"object": "DefinitelyNotHere"})
        payload = structured(missing)
        check(
            "missing object reports OBJECT_NOT_FOUND",
            missing.get("isError") is True
            and payload["error"]["code"] == "OBJECT_NOT_FOUND",
            str(payload),
        )
        check(
            "not-found error lists candidates",
            "available" in payload["error"].get("details", {}),
            str(payload),
        )

        unknown = client.request(
            "tools/call", {"name": "execute_python", "arguments": {"code": "1"}}
        )
        check(
            "there is no python execution tool",
            "error" in unknown,
            json.dumps(unknown)[:200],
        )

        # -- collections and selection ---------------------------------------
        collection = structured(
            client.call_tool("collection.create", {"name": "E2EProps", "objects": [cube_id]})
        )
        check("collection.create", collection["collection"]["object_count"] == 1, str(collection)[:200])

        client.call_tool("selection.set", {"objects": [cube_id], "active": cube_id})
        selection = structured(client.call_tool("selection.get"))
        check("selection round trip", selection["active"] == cube_id, str(selection))


        # -- batch execution --------------------------------------------------
        client.call_tool("tools.categories.enable", {"category": "materials"})
        batch = structured(
            client.call_tool(
                "batch.execute",
                {
                    "operations": [
                        {
                            "id": "wall",
                            "op": "object.create",
                            "args": {"type": "CUBE", "name": "BatchWall",
                                     "dimensions": {"x": 4, "y": 0.2, "z": 3}},
                        },
                        {
                            "id": "concrete",
                            "op": "material.create",
                            "args": {"name": "BatchConcrete",
                                     "principled": {"roughness": 0.9, "metallic": 0.0}},
                        },
                        {
                            "op": "material.assign",
                            "args": {
                                "material": {"result_of": "concrete"},
                                "objects": [{"result_of": "wall"}],
                            },
                        },
                    ]
                },
            )
        )
        check("batch.execute succeeded", batch["success"] is True, str(batch)[:400])
        check("batch ran every step", batch["completed"] == 3, str(batch)[:200])
        # The first two steps take literal arguments and forward straight to
        # Blender, so they must travel in a single frame; the third references
        # both of them and cannot. Coalescing is the whole reason batching is
        # faster than the same calls made one at a time, so if it silently stops
        # happening the numbers in the README quietly become wrong.
        check(
            "independent steps were coalesced into one dispatch",
            batch.get("dispatch_runs") == 1,
            f"dispatch_runs={batch.get('dispatch_runs')}",
        )

        wall_id = batch["results"][0]["result"]["object"]["id"]
        fetched = structured(client.call_tool("object.get", {"object": wall_id}))
        check(
            "typed references wired the material to the object",
            "BatchConcrete" in (fetched["object"].get("materials") or []),
            str(fetched["object"].get("materials")),
        )

        # A failure part-way through stops the batch and says where.
        failing = structured(
            client.call_tool(
                "batch.execute",
                {
                    "operations": [
                        {"op": "object.create", "args": {"type": "CUBE", "name": "BatchOk"}},
                        {"op": "object.get", "args": {"object": "NoSuchThing"}},
                        {"op": "object.create", "args": {"type": "CUBE", "name": "NeverMade"}},
                    ]
                },
            )
        )
        check("a failing batch reports failure", failing["success"] is False, str(failing)[:200])
        check("the failing step is identified", failing["failed_index"] == 1, str(failing)[:200])
        check("later steps did not run", failing["completed"] == 1, str(failing)[:200])
        objects = structured(client.call_tool("object.list", {"name_contains": "NeverMade"}))
        check("the step after the failure was skipped", objects["total"] == 0, str(objects)[:150])

        # BEST_EFFORT keeps going.
        best = structured(
            client.call_tool(
                "batch.execute",
                {
                    "mode": "BEST_EFFORT",
                    "operations": [
                        {"op": "object.get", "args": {"object": "StillMissing"}},
                        {"op": "object.create", "args": {"type": "CUBE", "name": "BestEffortMade"}},
                    ],
                },
            )
        )
        check("BEST_EFFORT continues past a failure", best["completed"] == 1, str(best)[:200])
        objects = structured(client.call_tool("object.list", {"name_contains": "BestEffortMade"}))
        check("the later step still ran", objects["total"] == 1, str(objects)[:150])

        # An unknown tool inside a batch fails the whole batch before anything runs.
        unknown = client.call_tool(
            "batch.execute",
            {"operations": [{"op": "object.create", "args": {"type": "CUBE", "name": "X"}},
                            {"op": "not.a.tool", "args": {}}]},
        )
        payload = structured(unknown)
        check(
            "an unknown tool aborts the batch up front",
            unknown.get("isError") is True and payload["error"]["details"]["index"] == 1,
            str(payload)[:250],
        )
        objects = structured(client.call_tool("object.list", {"name_contains": "X"}))
        check("nothing ran from the aborted batch", objects["total"] == 0, str(objects)[:150])

        # Atomic mode refuses operations that write outside the .blend file.
        client.call_tool("tools.categories.enable", {"category": "render"})
        atomic = client.call_tool(
            "batch.execute",
            {
                "mode": "ATOMIC",
                "operations": [
                    {"op": "object.create", "args": {"type": "CUBE", "name": "AtomicCube"}},
                    {"op": "render.execute", "args": {}},
                ],
            },
        )
        payload = structured(atomic)
        check(
            "atomic batches refuse external side effects",
            atomic.get("isError") is True
            and payload["error"]["code"] == "TRANSACTION_UNSUPPORTED",
            str(payload)[:300],
        )

        # A pure-mutation atomic batch is refused too in background Blender,
        # because there is no undo stack there -- and it says exactly that.
        atomic = client.call_tool(
            "batch.execute",
            {
                "mode": "ATOMIC",
                "operations": [
                    {"op": "object.create", "args": {"type": "CUBE", "name": "AtomicCube"}},
                ],
            },
        )
        payload = structured(atomic)
        check(
            "atomic mode explains the headless limitation",
            atomic.get("isError") is True
            and payload["error"]["code"] == "TRANSACTION_UNSUPPORTED"
            and "background" in payload["error"]["message"],
            str(payload)[:300],
        )


        # -- workflows --------------------------------------------------------
        client.call_tool("tools.categories.enable", {"category": "workflows"})
        client.call_tool("tools.categories.enable", {"category": "lights"})
        client.call_tool("tools.categories.enable", {"category": "geometry_nodes"})

        wall = structured(
            client.call_tool(
                "workflow.model.create_wall",
                {
                    "start": {"x": 0, "y": 0, "z": 0},
                    "end": {"x": 6, "y": 0, "z": 0},
                    "height": 3.0,
                    "thickness": 0.25,
                    "name": "SouthWall",
                },
            )
        )
        check("workflow.model.create_wall", wall["success"] is True, str(wall)[:400])
        check(
            "the wall geometry was computed in Rust first",
            wall["steps"][0].get("op") is None and "location" in wall["steps"][0]["result"],
            str(wall["steps"][0])[:250],
        )
        wall_id = wall["created"]["object"]["id"]
        fetched = structured(client.call_tool("object.get", {"object": wall_id}))
        dims = fetched["object"]["dimensions"]
        check(
            "the wall is the size that was asked for",
            abs(dims["x"] - 6.0) < 1e-3 and abs(dims["z"] - 3.0) < 1e-3,
            str(dims),
        )
        loc = fetched["object"]["location"]
        check(
            "the wall stands on its base at the midpoint",
            abs(loc["x"] - 3.0) < 1e-6 and abs(loc["z"] - 1.5) < 1e-6,
            str(loc),
        )

        # A wall that cannot exist is refused before Blender is touched.
        bad = client.call_tool(
            "workflow.model.create_wall",
            {"start": {"x": 0, "y": 0, "z": 0}, "end": {"x": 0, "y": 0, "z": 0},
             "height": 3.0, "thickness": 0.2},
        )
        check("an impossible wall is refused", bad.get("isError") is True, str(structured(bad))[:200])

        # PBR material, no textures: still one create plus one graph build.
        material = structured(
            client.call_tool(
                "workflow.material.pbr",
                {
                    "name": "WallConcrete",
                    "base_color": {"r": 0.55, "g": 0.54, "b": 0.5, "a": 1.0},
                    "roughness": 0.85,
                    "metallic": 0.0,
                    "assign_to": [wall_id],
                },
            )
        )
        check("workflow.material.pbr", material["success"] is True, str(material)[:400])
        ops = [step.get("op") for step in material["steps"] if step.get("op")]
        check(
            "the pbr workflow is three Blender calls",
            ops == ["material.create", "shader.graph.build", "material.assign"],
            str(ops),
        )
        fetched = structured(client.call_tool("object.get", {"object": wall_id}))
        check(
            "the material reached the wall",
            "WallConcrete" in (fetched["object"].get("materials") or []),
            str(fetched["object"].get("materials")),
        )

        # Three-point lighting, sized from the subject.
        lighting = structured(
            client.call_tool("workflow.lighting.three_point", {"target": wall_id})
        )
        check("workflow.lighting.three_point", lighting["success"] is True, str(lighting)[:400])
        for role in ("key_light", "fill_light", "rim_light"):
            check(f"{role} was created", role in lighting["created"], str(lighting["created"].keys()))
        # The bridge runs against Blender's normal startup file, which already
        # contains a light, so the rig's three are counted by name.
        lights = structured(client.call_tool("light.list", {}))
        rig = [l for l in lights["lights"] if l["name"] in ("Key", "Fill", "Rim")]
        check("the rig added three lights", len(rig) == 3, str([l["name"] for l in lights["lights"]]))

        key = next(l for l in rig if l["name"] == "Key")
        fill = next(l for l in rig if l["name"] == "Fill")
        check(
            "the key is brighter than the fill",
            key["energy"] > fill["energy"],
            f"key {key['energy']} fill {fill['energy']}",
        )

        # Scatter, planned in Rust and built in one call.
        ground = structured(
            client.call_tool(
                "object.create",
                {"type": "PLANE", "name": "Ground", "dimensions": {"x": 20, "y": 20, "z": 0}},
            )
        )
        rock = structured(
            client.call_tool("object.create", {"type": "ICO_SPHERE", "name": "Rock"})
        )
        scatter_result = client.call_tool(
                "geometry_nodes.scatter",
                {
                    "surface": ground["object"]["id"],
                    "source": {"object": rock["object"]["id"]},
                    "density": 0.5,
                    "seed": 7,
                    "scale_min": 0.6,
                    "scale_max": 1.4,
                    "align_to_normal": True,
                },
        )
        scatter = structured(scatter_result)
        check(
            "geometry_nodes.scatter",
            scatter_result.get("isError") is not True and scatter.get("success") is True,
            str(scatter)[:600],
        )
        ops = [step.get("op") for step in scatter["steps"] if step.get("op")]
        check(
            "the scatter is three Blender calls",
            ops == [
                "geometry_nodes.group.create",
                "geometry_nodes.graph.build",
                "geometry_nodes.modifier.attach",
            ],
            str(ops),
        )
        modifiers = structured(
            client.call_tool("geometry_nodes.modifier.list", {"object": ground["object"]["id"]})
        )
        check("the scatter modifier is attached", modifiers["total"] == 1, str(modifiers)[:200])

        # A workflow that fails part-way cleans up after itself.
        broken = client.call_tool(
            "geometry_nodes.scatter",
            {
                "surface": "NoSuchSurface",
                "source": {"object": rock["object"]["id"]},
                "density": 1.0,
            },
        )
        payload = structured(broken)
        check("a broken workflow reports failure", broken.get("isError") is True, str(payload)[:200])
        report = payload["error"]["details"]["report"]
        check(
            "the failed workflow rolled itself back",
            report["rollback"]["complete"] is True,
            str(report.get("rollback"))[:250],
        )
        groups = structured(client.call_tool("geometry_nodes.group.list", {}))
        check(
            "no orphan node group was left behind",
            groups["total"] == 1,
            str(groups)[:250],
        )

        # Export preparation, dry run.
        prepared = structured(
            client.call_tool(
                "workflow.export.prepare",
                {"profile": "GAME_ASSET", "dry_run": True},
            )
        )
        findings = prepared["created"]["findings"]
        codes = {f["code"] for f in findings}
        # Blender's primitives already carry UVs, so the problems a fresh scene
        # really has are unapplied scale and untriangulated quads.
        check(
            "workflow.export.prepare finds real problems",
            {"UNAPPLIED_SCALE", "NOT_TRIANGULATED"} <= codes,
            str(sorted(codes)),
        )
        check(
            "findings carry the entity they concern",
            all(f.get("entity") for f in findings),
            str(findings)[:200],
        )
        check(
            "every finding suggests a fix",
            all(f.get("suggested_fix") for f in findings),
            str(findings)[:300],
        )

        # -- cleanup ----------------------------------------------------------
        deleted = structured(client.call_tool("object.delete", {"objects": [cube_id]}))
        check("object.delete", len(deleted["deleted"]) == 1, str(deleted))

        gone = client.call_tool("object.get", {"object": cube_id})
        check("deleted object is gone", gone.get("isError") is True, str(structured(gone)))

    finally:
        client.close()
        if blender_process is not None:
            blender_process.terminate()
            try:
                blender_process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                blender_process.kill()
        shutil.rmtree(workspace, ignore_errors=True)

    print()
    print(f"passed: {PASSED}")
    if FAILED:
        print(f"failed: {len(FAILED)}")
        for name, detail in FAILED:
            print(f"  - {name}: {detail}")
        return 1
    print("end-to-end stack verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""The shortest path a new user takes, over real MCP, against a stock Blender.

Eleven steps: connect, make a cube, rename it, move it, bevel it, give it a
material, light it, point a camera at it, look at the result, and clean up. If
this fails, the project does not work; everything else is detail.

Blender is started with ``--factory-startup``, so no third-party add-on is
loaded and nothing but this repository's own bridge is involved.

    python tests/blender/test_standalone_smoke.py [--blender PATH] [--binary PATH]

Exits non-zero on failure, so it is usable from CI.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from test_bridge_roundtrip import (  # noqa: E402
    McpClient,
    find_binary,
    find_blender,
    free_port,
    structured,
)

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


def call(client: McpClient, tool: str, arguments: dict | None = None) -> dict:
    """Call a tool and insist it succeeded, so a failure names the step."""
    result = client.call_tool(tool, arguments or {})
    payload = structured(result)
    if result.get("isError"):
        raise RuntimeError(f"{tool} failed: {json.dumps(payload)[:400]}")
    return payload


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--blender", default=None)
    parser.add_argument("--binary", default=None)
    args = parser.parse_args()

    blender = find_blender(args.blender)
    binary = find_binary(args.binary)
    port = free_port()
    workspace = tempfile.mkdtemp(prefix="blender-mcp-standalone-")

    print(f"blender: {blender}")
    print(f"server:  {binary}")
    print(f"port:    {port}")

    # A deliberately bare environment: nothing configured beyond the port and
    # the workspace, so this is what a first-time user gets.
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("BLENDER_MCP_")
    }
    env.update(
        {
            "BLENDER_MCP_PORT": str(port),
            "BLENDER_MCP_WORKSPACE": workspace,
            "BLENDER_MCP_EAGER_TOOLS": "1",
            "BLENDER_MCP_LOG": "warn",
        }
    )

    client = McpClient([binary], env)
    blender_process = None
    try:
        # 1. connect --------------------------------------------------------
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "standalone-smoke", "version": "0.1.0"},
            },
        )
        client.notify("notifications/initialized")

        listed = client.request("tools/list", {})["result"]["tools"]
        names = [tool["name"] for tool in listed]
        check("the server lists its tools", len(names) > 200, str(len(names)))
        check(
            "no tool offers code execution",
            not [
                name
                for name in names
                for word in name.replace(".", "_").split("_")
                if word in {"python", "shell", "exec", "eval", "script", "subprocess"}
            ],
            str(names),
        )

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
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

        deadline = time.monotonic() + 90
        status = {}
        while time.monotonic() < deadline:
            status = call(client, "blender.status")
            if status.get("connected"):
                break
            time.sleep(0.5)
        check("Blender connects to the server", status.get("connected") is True, str(status))
        version = status.get("blender", {}).get("version") or status.get("blender_version")
        print(f"  ..   Blender reports {version}")

        capabilities = call(client, "blender.capabilities")
        check(
            "the build reports its own capabilities",
            bool(capabilities.get("render_engines")),
            str(capabilities)[:200],
        )

        # 2. create a cube --------------------------------------------------
        cube = call(client, "object.create", {"type": "CUBE", "name": "SmokeCube"})
        cube_id = cube["object"]["id"]
        check("a cube is created", cube["object"]["name"] == "SmokeCube", str(cube)[:200])

        # 3. rename it ------------------------------------------------------
        call(client, "object.rename", {"object": cube_id, "name": "SmokeCrate"})
        renamed = call(client, "object.get", {"object": cube_id})
        check(
            "renaming does not invalidate the id",
            renamed["object"]["name"] == "SmokeCrate",
            str(renamed)[:200],
        )

        # 4. transform it ---------------------------------------------------
        call(
            client,
            "object.transform",
            {
                "object": cube_id,
                "location": {"x": 1.0, "y": 2.0, "z": 0.5},
                "scale": {"x": 1.0, "y": 1.0, "z": 2.0},
            },
        )
        moved = call(client, "object.get", {"object": cube_id})
        location = moved["object"]["location"]
        check(
            "the transform is applied",
            abs(location["x"] - 1.0) < 1e-6 and abs(location["z"] - 0.5) < 1e-6,
            str(location),
        )

        # 5. bevel it -------------------------------------------------------
        bevel = call(
            client,
            "modifier.add",
            {
                "object": cube_id,
                "type": "BEVEL",
                "name": "Edges",
                "properties": [
                    {"name": "width", "value": {"float": 0.02}},
                    {"name": "segments", "value": {"int": 3}},
                ],
            },
        )
        check("a bevel modifier is added", bevel.get("modifier") is not None, str(bevel)[:200])

        # 6. a generic Principled material ----------------------------------
        material = call(
            client,
            "material.create",
            {
                "name": "SmokePaint",
                "principled": {
                    "base_color": {"r": 0.8, "g": 0.2, "b": 0.1, "a": 1.0},
                    "roughness": 0.35,
                    "metallic": 0.0,
                },
            },
        )
        check(
            "a Principled material is created",
            material["material"]["name"] == "SmokePaint",
            str(material)[:200],
        )

        # 7. assign it ------------------------------------------------------
        call(
            client,
            "material.assign",
            {"material": material["material"]["id"], "objects": [cube_id]},
        )
        with_material = call(client, "object.get", {"object": cube_id})
        check(
            "the material reaches the object",
            "SmokePaint" in (with_material["object"].get("materials") or []),
            str(with_material["object"].get("materials")),
        )

        # 8. an area light ---------------------------------------------------
        light = call(
            client,
            "light.create",
            {
                "type": "AREA",
                "name": "SmokeKey",
                "location": {"x": 3.0, "y": -3.0, "z": 4.0},
                "energy": 400.0,
                "target": cube_id,
            },
        )
        check("an area light is created", light["light"]["type"] == "AREA", str(light)[:200])

        # 9. a camera --------------------------------------------------------
        camera = call(
            client,
            "camera.create",
            {
                "name": "SmokeCam",
                "frame_objects": [cube_id],
                "set_active": True,
            },
        )
        camera_id = camera["camera"]["id"]
        framed = call(
            client,
            "camera.auto_frame",
            {"camera": camera_id, "objects": [cube_id], "padding": 0.15},
        )
        check("the camera frames the object", framed is not None, str(framed)[:200])

        # 10. look at the result ---------------------------------------------
        summary = call(client, "scene.summary")
        check(
            "the scene holds what was built",
            summary["objects"]["total"] >= 3,
            str(summary)[:300],
        )
        statistics = call(client, "scene.statistics")
        check(
            "statistics count the mesh",
            statistics["objects"]["mesh"] >= 1 and statistics["objects"]["light"] >= 1,
            str(statistics)[:300],
        )
        surfaces = call(
            client,
            "scene.surface.inspect",
            {"object": cube_id, "classification": "WALL"},
        )
        check(
            "surface inspection groups the cube's walls",
            surfaces["total"] == 4,
            f"expected the four vertical faces, got {surfaces['total']}",
        )

        # 11. clean up --------------------------------------------------------
        deleted = call(
            client,
            "object.delete",
            {"objects": [cube_id, light["light"]["id"], camera_id]},
        )
        check("the test objects are deleted", len(deleted["deleted"]) == 3, str(deleted)[:200])
        purged = call(client, "scene.cleanup", {"purge_orphans": True})
        check("orphaned data is purged", purged is not None, str(purged)[:200])

    finally:
        client.close()
        if blender_process is not None:
            blender_process.terminate()
            try:
                blender_process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                blender_process.kill()

    print()
    if FAILED:
        print(f"passed: {PASSED}, failed: {len(FAILED)}")
        for name, detail in FAILED:
            print(f"  FAIL {name}: {detail}")
        return 1
    print(f"passed: {PASSED}")
    print("standalone smoke scenario verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

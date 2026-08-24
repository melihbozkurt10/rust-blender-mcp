"""End-to-end asset test: MCP client -> Rust server -> Poly Haven -> Blender.

The only test in the suite that touches both the network and Blender. It proves
the part that unit tests cannot: that a real provider response becomes real
files on disk, and that those files become a world environment and a material
inside a real Blender.

    python tests/blender/test_asset_import.py [--blender PATH] [--binary PATH]

Downloads roughly 10 MB from Poly Haven (1k variants). Everything Poly Haven
publishes is CC0. Exits non-zero on failure.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
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


def error_code(result: dict) -> str | None:
    """The error code of a failed tool result, if it failed."""
    if not result.get("isError"):
        return None
    payload = structured(result)
    if isinstance(payload, dict):
        return payload.get("code")
    text = json.dumps(result)
    return text


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--blender", default=None)
    parser.add_argument("--binary", default=None)
    args = parser.parse_args()

    blender = find_blender(args.blender)
    binary = find_binary(args.binary)
    port = free_port()
    workspace = tempfile.mkdtemp(prefix="blender-mcp-assets-")

    print(f"blender:   {blender}")
    print(f"server:    {binary}")
    print(f"workspace: {workspace}")

    env = dict(os.environ)
    env.update(
        {
            "BLENDER_MCP_PORT": str(port),
            "BLENDER_MCP_WORKSPACE": workspace,
            "BLENDER_MCP_EAGER_TOOLS": "1",
            "BLENDER_MCP_LOG": "warn",
        }
    )
    # The token must never be needed for this test: Poly Haven is public, and a
    # test that silently depended on a credential would pass on one machine and
    # fail on every other.
    env.pop("BLENDER_MCP_SKETCHFAB_TOKEN", None)

    client = McpClient([binary], env)
    blender_process = None
    try:
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "blender-mcp-assets", "version": "0.1.0"},
            },
        )
        client.notify("notifications/initialized")

        # -- providers, before Blender is even running ----------------------
        providers = structured(client.call_tool("asset.providers"))
        ids = [p["id"] for p in providers["providers"]]
        check("polyhaven is configured", "polyhaven" in ids, str(ids))
        check("sketchfab is listed", "sketchfab" in ids, str(ids))

        sketchfab = next(p for p in providers["providers"] if p["id"] == "sketchfab")
        check(
            "sketchfab reports it has no credentials",
            sketchfab["requires_auth"] is True and sketchfab["authenticated"] is False,
            str(sketchfab),
        )
        check("downloads are enabled", providers["downloads_enabled"] is True, str(providers))
        check(
            "the provider list carries a licence notice",
            "licence" in providers["notice"].lower() or "license" in providers["notice"].lower(),
            providers["notice"],
        )

        # -- search ----------------------------------------------------------
        found = structured(
            client.call_tool(
                "asset.search",
                {
                    "provider": "polyhaven",
                    "query": "studio",
                    "asset_type": "HDRI",
                    "limit": 5,
                },
                timeout=120,
            )
        )
        check("search returned results", len(found["assets"]) > 0, str(found)[:300])
        check(
            "search reports which providers it asked",
            found["providers_searched"] == ["polyhaven"],
            str(found.get("providers_searched")),
        )

        hdri = found["assets"][0]
        check("results carry a stable id", bool(hdri["id"]), str(hdri)[:200])
        check(
            "results carry the provider's licence verbatim",
            hdri["license"]["id"] == "CC0" and hdri["license"]["commercial_use"] is True,
            str(hdri.get("license")),
        )
        check(
            "no result claims to be free to use",
            "free_to_use" not in json.dumps(hdri),
            str(hdri)[:200],
        )

        # A search across every provider must survive Sketchfab having no token.
        everywhere = structured(
            client.call_tool(
                "asset.search",
                {"query": "concrete", "asset_type": "TEXTURE", "limit": 3},
                timeout=120,
            )
        )
        check(
            "an unconfigured provider does not sink the search",
            len(everywhere["assets"]) > 0,
            str(everywhere)[:300],
        )

        # -- security, before anything is downloaded -------------------------
        traversal = client.call_tool(
            "asset.download",
            {"provider": "polyhaven", "asset_id": "../../../../etc/passwd"},
        )
        check(
            "a traversal asset id is refused",
            traversal.get("isError") is True,
            str(structured(traversal))[:200],
        )

        unknown = client.call_tool("asset.get", {"provider": "nosuch", "asset_id": "x"})
        check(
            "an unknown provider is refused",
            unknown.get("isError") is True,
            str(structured(unknown))[:200],
        )

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
                "420",
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

        # -- an HDRI becomes the world environment ---------------------------
        raw = client.call_tool(
            "asset.import",
            {
                "provider": "polyhaven",
                "asset_id": hdri["provider_id"],
                "resolution": 1024,
                "format": "hdr",
                "apply_as_world": True,
            },
            timeout=300,
        )
        if raw.get("isError"):
            check("asset.import succeeded", False, json.dumps(structured(raw))[:600])
            raise SystemExit(1)
        imported = structured(raw)
        check(
            "asset.import applied the HDRI to the world",
            imported["applied"]["action"] == "world_environment",
            str(imported.get("applied")),
        )
        check("the download is reported as fresh", imported["from_cache"] is False, str(imported)[:200])
        check(
            "the file landed under the downloads root",
            imported["files"][0]["path"].startswith("polyhaven/"),
            str(imported["files"][0]),
        )
        check(
            "every file carries a checksum",
            all(len(f["sha256"]) == 64 for f in imported["files"]),
            str(imported["files"])[:300],
        )
        check(
            "the import result repeats the licence",
            imported["license"]["id"] == "CC0",
            str(imported.get("license")),
        )

        on_disk = os.path.join(workspace, "downloads", *imported["files"][0]["path"].split("/"))
        check("the file really exists", os.path.isfile(on_disk), on_disk)
        check(
            "nothing executable was written",
            not on_disk.endswith((".py", ".exe", ".sh", ".dll")),
            on_disk,
        )

        world = structured(client.call_tool("scene.world.get"))
        check("the world now has an environment texture", bool(world["world"].get("hdri")), str(world))

        # -- the second import is served from the cache ----------------------
        again = structured(
            client.call_tool(
                "asset.import",
                {
                    "provider": "polyhaven",
                    "asset_id": hdri["provider_id"],
                    "resolution": 1024,
                    "format": "hdr",
                    "apply_as_world": True,
                },
                timeout=180,
            )
        )
        check("the second import hits the cache", again["from_cache"] is True, str(again)[:200])
        check(
            "the cached checksum matches the downloaded one",
            again["files"][0]["sha256"] == imported["files"][0]["sha256"],
            str(again["files"][0]),
        )

        # -- a texture set becomes a material --------------------------------
        raw_texture = client.call_tool(
            "asset.import",
            {
                "provider": "polyhaven",
                "asset_id": "rocks_ground_02",
                "resolution": 1024,
                "format": "jpg",
                "build_material": True,
                "name": "Rocks Ground",
            },
            timeout=420,
        )
        if raw_texture.get("isError"):
            check("the texture import succeeded", False, json.dumps(structured(raw_texture))[:600])
            raise SystemExit(1)
        texture = structured(raw_texture)
        check(
            "asset.import built a material",
            texture["applied"]["action"] == "built_material",
            str(texture.get("applied"))[:300],
        )
        maps = texture["applied"]["maps"]
        check("the material wired several maps", len(maps) >= 3, str(maps))
        check(
            "one resolution across the whole set",
            all("_1k." in f["path"] for f in texture["files"]),
            str([f["path"] for f in texture["files"]]),
        )

        material = structured(
            client.call_tool("material.get", {"material": texture["applied"]["material"]})
        )
        check("the material exists in Blender", bool(material["material"]["name"]), str(material)[:200])

        tree = structured(
            client.call_tool("shader.tree.get", {"material": texture["applied"]["material"]})
        )
        node_types = [n["type"] for n in tree["tree"]["nodes"]]
        check(
            "the graph has image texture nodes",
            node_types.count("ShaderNodeTexImage") >= 3,
            str(node_types),
        )
        check(
            "the graph has a normal map node",
            "ShaderNodeNormalMap" in node_types,
            str(node_types),
        )
        # The AO map multiplies base colour through a Mix node whose sockets
        # all share names. If the identifiers were wrong the graph would not
        # have built at all, so its presence is the check.
        check(
            "the AO map went through a mix node",
            "ShaderNodeMix" in node_types,
            str(node_types),
        )
        links = tree["tree"]["links"]
        # Sockets are reported by identifier, which on the Principled BSDF is
        # `Base Color`.
        check(
            "something drives the shader's base colour",
            any(link["to_socket"] == "Base Color" for link in links),
            str(links)[:400],
        )
        check(
            "every link Blender made is valid",
            all(link["is_valid"] for link in links),
            str([l for l in links if not l["is_valid"]]),
        )

        # The single most common texturing mistake, checked in the one place it
        # can actually be observed: inside Blender, on the loaded image.
        images = structured(client.call_tool("image.list", {"limit": 100}))
        data_maps = [
            image
            for image in images["images"]
            if any(token in image["name"] for token in ("nor_gl", "Rough", "Displacement", "AO"))
        ]
        check("data maps were loaded", len(data_maps) >= 2, str([i["name"] for i in images["images"]]))
        wrong = [i["name"] for i in data_maps if i.get("colorspace") != "Non-Color"]
        check("every data map is Non-Color", not wrong, str(wrong))

        colour_maps = [i for i in images["images"] if "Diffuse" in i["name"]]
        check(
            "the colour map is not Non-Color",
            all(i.get("colorspace") != "Non-Color" for i in colour_maps),
            str([(i["name"], i.get("colorspace")) for i in colour_maps]),
        )

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
    print("asset pipeline verified end to end")
    return 0


if __name__ == "__main__":
    sys.exit(main())

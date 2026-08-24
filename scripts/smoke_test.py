"""Exercise the bridge's handlers inside a real Blender, with no socket.

Run with::

    blender --background --factory-startup --python scripts/smoke_test.py

Handlers are called through :func:`dispatcher.dispatch`, the same entry point
the socket path uses, so this covers argument decoding, error mapping and the
handlers themselves. What it deliberately does not cover is the transport --
``tests/blender/test_bridge_roundtrip.py`` does that end to end.

Exits non-zero if anything fails, so it is usable from CI.
"""

from __future__ import annotations

import json
import math
import os
import sys
import tempfile
import traceback
import uuid
from typing import Any

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)

import bpy  # noqa: E402

from blender_extension import dispatcher  # noqa: E402
from blender_extension import queue as queue_module  # noqa: E402
from blender_extension import operations  # noqa: E402  (registers handlers)

PASSED = 0
FAILED: list[tuple[str, str]] = []


def call(op: str, args: dict | None = None, *, expect_error: str | None = None) -> Any:
    """Run one operation and check the outcome."""
    global PASSED

    frame = {
        "type": "request",
        "request_id": str(uuid.uuid4()),
        "command": {"op": op, "args": args or {}},
    }
    response = dispatcher.dispatch(frame)

    if expect_error is not None:
        if response.get("ok"):
            FAILED.append((op, f"expected error {expect_error}, but the call succeeded"))
            return None
        actual = response.get("error", {}).get("code")
        if actual != expect_error:
            FAILED.append((op, f"expected error {expect_error}, got {actual}"))
            return None
        PASSED += 1
        return response["error"]

    if not response.get("ok"):
        error = response.get("error", {})
        FAILED.append((op, f"{error.get('code')}: {error.get('message')}"))
        return None

    PASSED += 1
    return response.get("result")


def check(name: str, condition: bool, detail: str = "") -> None:
    global PASSED
    if condition:
        PASSED += 1
    else:
        FAILED.append((name, detail or "assertion failed"))


def reset_scene() -> None:
    bpy.ops.wm.read_factory_settings(use_empty=True)


def test_materials_and_shaders() -> None:
    """Materials, the Principled BSDF and generic shader graph editing."""
    reset_scene()
    cube = call("object.create", {"type": "CUBE", "name": "Shaded"})
    cube_id = cube["object"]["id"] if cube else None

    created = call(
        "material.create",
        {
            "name": "Concrete",
            "principled": {
                "base_color": {"r": 0.4, "g": 0.4, "b": 0.42, "a": 1.0},
                "roughness": 0.85,
                "metallic": 0.0,
            },
            "assign_to": [cube_id],
        },
    )
    check("material.create", created is not None and created["material"]["name"] == "Concrete")
    material_id = created["material"]["id"] if created else None
    check(
        "material.principled roundtrip",
        created is not None
        and abs(created["material"]["principled"]["roughness"] - 0.85) < 1e-6,
        str(created["material"].get("principled") if created else None),
    )
    check(
        "material assigned on create",
        created is not None and len(created["assigned"]) == 1,
        str(created["assigned"] if created else None),
    )

    updated = call(
        "material.update",
        {"material": material_id, "principled": {"emission_strength": 3.0, "ior": 1.45}},
    )
    check(
        "material.update",
        updated is not None and "emission_strength" in updated["changed"],
        str(updated["changed"] if updated else None),
    )

    slots = call("material.slot.list", {"object": cube_id})
    check("material.slot.list", slots is not None and len(slots["slots"]) == 1)

    # Deleting a material that is still in use must be refused by default.
    call("material.delete", {"material": material_id}, expect_error="INVALID_ARGUMENT")

    # -- generic shader graph -------------------------------------------
    tree = call("shader.tree.get", {"material": material_id, "include_socket_defaults": True})
    check(
        "shader.tree.get",
        tree is not None and len(tree["tree"]["nodes"]) >= 2,
        str(tree)[:200] if tree else None,
    )
    principled = next(
        (n for n in tree["tree"]["nodes"] if n["type"] == "ShaderNodeBsdfPrincipled"), None
    )
    check("shader tree has a principled node", principled is not None)

    noise = call(
        "shader.node.create",
        {
            "material": material_id,
            "node_type": "ShaderNodeTexNoise",
            "name": "Grain",
            "location": {"x": -400.0, "y": 0.0},
            "inputs": [{"name": "Scale", "value": {"float": 12.0}}],
        },
    )
    check("shader.node.create", noise is not None and noise["node"]["type"] == "ShaderNodeTexNoise")
    noise_id = noise["node"]["id"] if noise else None

    linked = call(
        "shader.link.create",
        {
            "material": material_id,
            "from": {"node": noise_id, "name": "Fac", "direction": "output"},
            "to": {"node": principled["id"], "name": "Roughness", "direction": "input"},
        },
    )
    check("shader.link.create", linked is not None, str(linked))

    links = call("shader.link.list", {"material": material_id})
    check(
        "shader.link.list sees the new link",
        links is not None and any(link["to_socket"] == "Roughness" for link in links["links"]),
        str(links)[:300] if links else None,
    )

    # Setting a default on a linked socket is refused unless forced.
    call(
        "shader.socket.set_default",
        {"material": material_id, "node": principled["id"], "name": "Roughness", "value": {"float": 0.2}},
        expect_error="INVALID_ARGUMENT",
    )

    # An unknown socket name comes back with the list of real ones.
    error = call(
        "shader.socket.set_default",
        {"material": material_id, "node": principled["id"], "name": "Shininess", "value": {"float": 1.0}},
        expect_error="INVALID_NODE_SOCKET",
    )
    check(
        "socket error lists the real sockets",
        error is not None and "available_inputs" in error.get("details", {}),
        str(error),
    )

    # An unknown node type is refused, not silently ignored.
    call(
        "shader.node.create",
        {"material": material_id, "node_type": "ShaderNodeNotAThing"},
        expect_error="INVALID_NODE_TYPE",
    )

    # An unknown property is refused with the real property list.
    error = call(
        "shader.node.update",
        {
            "material": material_id,
            "node": noise_id,
            "properties": [{"name": "shininess", "value": {"float": 1.0}}],
        },
        expect_error="INVALID_PROPERTY",
    )
    check(
        "property error lists real properties",
        error is not None and "available" in error.get("details", {}),
        str(error),
    )

    # Dunder access is refused outright.
    call(
        "shader.node.update",
        {
            "material": material_id,
            "node": noise_id,
            "properties": [{"name": "__class__", "value": {"string": "x"}}],
        },
        expect_error="PERMISSION_DENIED",
    )

    call("shader.link.delete", {"material": material_id, "to": {"node": principled["id"], "name": "Roughness"}})
    links = call("shader.link.list", {"material": material_id})
    check(
        "shader.link.delete",
        links is not None and not any(link["to_socket"] == "Roughness" for link in links["links"]),
    )

    call("shader.node.delete", {"material": material_id, "node": noise_id})
    tree = call("shader.tree.get", {"material": material_id})
    check(
        "shader.node.delete",
        tree is not None and not any(n["id"] == noise_id for n in tree["tree"]["nodes"]),
    )

    # The output node is protected from casual deletion.
    output = next((n for n in tree["tree"]["nodes"] if "Output" in n["type"]), None)
    if output is not None:
        call(
            "shader.node.delete",
            {"material": material_id, "node": output["id"]},
            expect_error="INVALID_ARGUMENT",
        )


def test_lights() -> None:
    reset_scene()
    target = call("object.create", {"type": "CUBE", "name": "Subject"})
    target_id = target["object"]["id"] if target else None

    key = call(
        "light.create",
        {
            "type": "AREA",
            "name": "Key",
            "location": {"x": 4.0, "y": -4.0, "z": 3.0},
            "target": target_id,
            "energy": 500.0,
            "temperature": 5600.0,
            "size": 2.0,
            "shape": "SQUARE",
        },
    )
    check("light.create", key is not None and key["light"]["type"] == "AREA")
    key_id = key["light"]["id"] if key else None
    check(
        "light aimed at the target",
        key is not None and abs(key["light"]["rotation_euler"]["x"]) > 1e-6,
        str(key["light"]["rotation_euler"] if key else None),
    )
    check(
        "temperature became a warm-ish colour",
        key is not None and key["light"]["color"]["r"] >= key["light"]["color"]["b"],
        str(key["light"]["color"] if key else None),
    )

    updated = call("light.update", {"light": key_id, "energy": 250.0, "spot_blend": 0.5})
    check(
        "light.update ignores settings that do not apply",
        updated is not None and "energy" in updated["changed"] and "spot_blend" not in updated["changed"],
        str(updated["changed"] if updated else None),
    )

    # Range checks live in the Rust server, not here: Blender itself accepts a
    # negative energy without complaint, so the bridge would have to duplicate
    # the rule. `tests/protocol` covers the Rust side.

    listed = call("light.list", {"light_type": "AREA"})
    check("light.list filter", listed is not None and listed["total"] == 1, str(listed))

    call("light.look_at", {"light": key_id, "target": target_id, "distance": 8.0})
    fetched = call("light.get", {"light": key_id})
    distance = 0.0
    if fetched:
        location = fetched["light"]["location"]
        distance = (location["x"] ** 2 + location["y"] ** 2 + location["z"] ** 2) ** 0.5
    check("light.look_at distance", abs(distance - 8.0) < 1e-3, f"distance was {distance}")

    call("light.delete", {"light": key_id})
    call("light.get", {"light": key_id}, expect_error="OBJECT_NOT_FOUND")


def test_modifiers() -> None:
    reset_scene()
    cube = call("object.create", {"type": "CUBE", "name": "Modified"})
    cube_id = cube["object"]["id"] if cube else None
    cutter = call("object.create", {"type": "UV_SPHERE", "name": "Cutter"})
    cutter_id = cutter["object"]["id"] if cutter else None

    subsurf = call(
        "modifier.add",
        {
            "object": cube_id,
            "type": "SUBSURF",
            "name": "Smooth",
            "properties": [{"name": "levels", "value": {"int": 2}}],
        },
    )
    check("modifier.add", subsurf is not None and subsurf["modifier"]["type"] == "SUBSURF")
    check(
        "modifier property applied",
        subsurf is not None
        and any(
            p["name"] == "levels" and p["value"].get("int") == 2
            for p in subsurf["modifier"]["properties"]
        ),
        str(subsurf["modifier"]["properties"] if subsurf else None),
    )

    boolean = call(
        "modifier.add",
        {"object": cube_id, "type": "BOOLEAN", "name": "Cut", "target": cutter_id},
    )
    check("modifier.add with target", boolean is not None and boolean["modifier"]["target"] == "Cutter")

    listed = call("modifier.list", {"object": cube_id})
    check("modifier.list", listed is not None and listed["total"] == 2)

    moved = call("modifier.move", {"object": cube_id, "modifier": "Cut", "to": "first"})
    check("modifier.move", moved is not None and moved["to_index"] == 0, str(moved))

    call(
        "modifier.update",
        {
            "object": cube_id,
            "modifier": "Smooth",
            "properties": [{"name": "not_a_property", "value": {"int": 1}}],
        },
        expect_error="INVALID_PROPERTY",
    )

    copied = call("modifier.copy", {"from": cube_id, "to": [cutter_id]})
    check("modifier.copy", copied is not None and len(copied["copied"][0]["modifiers"]) == 2)

    call("modifier.apply", {"object": cube_id, "modifier": "Smooth"})
    listed = call("modifier.list", {"object": cube_id})
    check("modifier.apply removed it from the stack", listed is not None and listed["total"] == 1)

    call("modifier.remove", {"object": cube_id, "modifier": "Cut"})
    listed = call("modifier.list", {"object": cube_id})
    check("modifier.remove", listed is not None and listed["total"] == 0)

    call(
        "modifier.get",
        {"object": cube_id, "modifier": "Nope"},
        expect_error="MODIFIER_NOT_FOUND",
    )


def test_mesh() -> None:
    reset_scene()
    cube = call("object.create", {"type": "CUBE", "name": "Editable"})
    cube_id = cube["object"]["id"] if cube else None

    info = call("mesh.info", {"object": cube_id})
    check("mesh.info", info is not None and info["vertices"] == 8 and info["faces"] == 6, str(info))
    revision = info["mesh_revision"] if info else 0

    verts = call("mesh.vertices.get", {"object": cube_id, "limit": 4})
    check("mesh.vertices.get", verts is not None and len(verts["vertices"]) == 4)
    check("mesh.vertices.get paginates", verts is not None and verts["next_cursor"] == "4")

    faces = call("mesh.faces.get", {"object": cube_id, "include_normals": True})
    check(
        "mesh.faces.get",
        faces is not None and len(faces["faces"]) == 6 and "normal" in faces["faces"][0],
    )

    # A stale revision must be refused rather than applied to the wrong faces.
    call(
        "mesh.extrude",
        {
            "object": cube_id,
            "selection": {"type": "FACE", "indices": [0], "expected_mesh_revision": revision + 99},
            "along_normal": 1.0,
        },
        expect_error="TOPOLOGY_STALE",
    )

    extruded = call(
        "mesh.extrude",
        {
            "object": cube_id,
            "selection": {"type": "FACE", "indices": [0], "expected_mesh_revision": revision},
            "along_normal": 1.0,
        },
    )
    check("mesh.extrude", extruded is not None and extruded["counts"]["faces"] > 6, str(extruded))
    check(
        "extrude bumps the revision",
        extruded is not None and extruded["mesh_revision"] > revision,
        str(extruded),
    )
    revision = extruded["mesh_revision"] if extruded else 0

    inset = call(
        "mesh.inset",
        {"object": cube_id, "selection": {"type": "FACE", "indices": [0]}, "thickness": 0.2},
    )
    check("mesh.inset", inset is not None and inset["created"] > 0, str(inset))

    beveled = call(
        "mesh.bevel",
        {"object": cube_id, "selection": {"type": "EDGE", "indices": [0, 1]}, "amount": 0.05,
         "segments": 2},
    )
    check("mesh.bevel", beveled is not None, str(beveled))

    subdivided = call("mesh.subdivide", {"object": cube_id, "cuts": 1})
    check(
        "mesh.subdivide",
        subdivided is not None and subdivided["counts"]["vertices"] > inset["counts"]["vertices"],
        str(subdivided),
    )

    triangulated = call("mesh.triangulate", {"object": cube_id})
    check("mesh.triangulate", triangulated is not None, str(triangulated))
    analysis = call("mesh.analyze", {"object": cube_id})
    check(
        "triangulate leaves only tris",
        analysis is not None and analysis["quads"] == 0 and analysis["ngons"] == 0,
        f"quads={analysis['quads']} ngons={analysis['ngons']}" if analysis else None,
    )

    joined = call("mesh.quads_from_tris", {"object": cube_id})
    check("mesh.quads_from_tris", joined is not None and joined["joined"] > 0, str(joined))

    call("mesh.normals.recalculate", {"object": cube_id})
    flipped = call("mesh.normals.flip", {"object": cube_id})
    check("mesh.normals.flip", flipped is not None and flipped["flipped"] > 0, str(flipped))

    # Loop cut on a fresh grid, where rings are well defined.
    reset_scene()
    plane = call("object.create", {"type": "PLANE", "name": "Grid"})
    plane_id = plane["object"]["id"] if plane else None
    call("mesh.subdivide", {"object": plane_id, "cuts": 3})
    before = call("mesh.info", {"object": plane_id})
    cut = call("mesh.loop_cut", {"object": plane_id, "edge_index": 0, "cuts": 1})
    after = call("mesh.info", {"object": plane_id})
    check(
        "mesh.loop_cut adds geometry",
        cut is not None and after is not None and after["vertices"] > before["vertices"],
        str(cut),
    )
    check("loop cut walked a ring", cut is not None and cut["ring_size"] >= 1, str(cut))

    # Merging by distance on a mesh with duplicates.
    reset_scene()
    built = call(
        "mesh.create",
        {
            "name": "Doubled",
            "vertices": [
                {"x": 0, "y": 0, "z": 0},
                {"x": 1, "y": 0, "z": 0},
                {"x": 1, "y": 1, "z": 0},
                {"x": 0, "y": 1, "z": 0},
                {"x": 0.0000001, "y": 0, "z": 0},
            ],
            "faces": [[0, 1, 2, 3]],
        },
    )
    check("mesh.create", built is not None and built["object"]["mesh"]["vertices"] == 5, str(built))
    built_id = built["object"]["id"] if built else None
    merged = call("mesh.merge_vertices", {"object": built_id, "distance": 0.001})
    check("mesh.merge_vertices", merged is not None and merged["merged"] == 1, str(merged))

    # Deleting everything must be refused without explicit indices.
    call(
        "mesh.delete_elements",
        {"object": built_id, "selection": {"type": "FACE"}, "mode": "FACES"},
        expect_error="INVALID_ARGUMENT",
    )
    deleted = call(
        "mesh.delete_elements",
        {"object": built_id, "selection": {"type": "FACE", "indices": [0]}, "mode": "ONLY_FACE"},
    )
    check("mesh.delete_elements", deleted is not None and deleted["counts"]["faces"] == 0, str(deleted))

    filled = call(
        "mesh.fill",
        {"object": built_id, "selection": {"type": "EDGE", "indices": [0, 1, 2, 3]}},
    )
    check("mesh.fill", filled is not None and filled["created"] >= 1, str(filled))

    # Out-of-range indices report the real element count.
    error = call(
        "mesh.inset",
        {"object": built_id, "selection": {"type": "FACE", "indices": [9999]}, "thickness": 0.1},
        expect_error="INVALID_ARGUMENT",
    )
    check(
        "out-of-range indices are reported precisely",
        error is not None and "out_of_range" in error.get("details", {}),
        str(error),
    )

    # Non-mesh objects are refused with a clear reason.
    empty = call("object.create", {"type": "EMPTY", "name": "NotAMesh"})
    call(
        "mesh.info",
        {"object": empty["object"]["id"]},
        expect_error="UNSUPPORTED_OPERATION",
    )


def test_camera() -> None:
    reset_scene()
    subject = call(
        "object.create",
        {"type": "CUBE", "name": "Hero", "dimensions": {"x": 2.0, "y": 2.0, "z": 4.0}},
    )
    subject_id = subject["object"]["id"] if subject else None

    created = call(
        "camera.create",
        {
            "name": "Shot",
            "location": {"x": 10.0, "y": -10.0, "z": 6.0},
            "set_active": True,
            "lens": {"millimetres": 50.0},
            "clip_start": 0.05,
            "clip_end": 500.0,
        },
    )
    check("camera.create", created is not None and created["camera"]["is_active"] is True)
    check(
        "camera lens applied",
        created is not None and abs(created["camera"]["lens_mm"] - 50.0) < 1e-6,
        str(created["camera"]["lens_mm"] if created else None),
    )
    camera_id = created["camera"]["id"] if created else None

    fov = call("camera.update", {"camera": camera_id, "lens": {"fov_degrees": 60.0}})
    check("camera lens by field of view", fov is not None and "lens" in fov["changed"], str(fov))

    framed = call(
        "camera.auto_frame",
        {"camera": camera_id, "objects": [subject_id], "padding": 0.15, "focus": True},
    )
    check("camera.auto_frame", framed is not None and framed["distance"] > 0, str(framed))
    check(
        "auto_frame centres on the subject",
        framed is not None and abs(framed["center"]["z"] - 0.0) < 1e-6,
        str(framed["center"] if framed else None),
    )
    # The camera must end up roughly `distance` away from the subject centre.
    if framed:
        location = call("camera.get", {"camera": camera_id})["camera"]["location"]
        centre = framed["center"]
        actual = (
            (location["x"] - centre["x"]) ** 2
            + (location["y"] - centre["y"]) ** 2
            + (location["z"] - centre["z"]) ** 2
        ) ** 0.5
        check(
            "auto_frame placed the camera at the computed distance",
            abs(actual - framed["distance"]) < 1e-3,
            f"expected {framed['distance']}, got {actual}",
        )

    tracked = call("camera.track_object", {"camera": camera_id, "target": subject_id})
    check("camera.track_object", tracked is not None and tracked["constraint"] == "TRACK_TO")
    # Tracking twice must replace, not stack.
    call("camera.track_object", {"camera": camera_id, "target": subject_id, "constraint": "DAMPED_TRACK"})
    fetched = call("camera.get", {"camera": camera_id})
    check(
        "tracking replaces rather than stacks",
        fetched is not None and len(fetched["camera"].get("constraints", [])) == 1,
        str(fetched["camera"].get("constraints") if fetched else None),
    )

    cleared = call("camera.clear_tracking", {"camera": camera_id})
    check("camera.clear_tracking", cleared is not None and len(cleared["removed"]) == 1)

    dof = call(
        "camera.depth_of_field.update",
        {"camera": camera_id, "enabled": True, "f_stop": 1.8, "focus_object": subject_id},
    )
    check(
        "camera.depth_of_field.update",
        dof is not None and dof["camera"]["depth_of_field"]["enabled"] is True,
        str(dof["camera"]["depth_of_field"] if dof else None),
    )

    call("camera.track_object", {"camera": camera_id, "target": camera_id},
         expect_error="INVALID_ARGUMENT")

    listed = call("camera.list", {})
    check("camera.list", listed is not None and listed["total"] == 1)


def test_render() -> None:
    import tempfile

    reset_scene()
    call("object.create", {"type": "CUBE", "name": "Subject"})
    camera = call(
        "camera.create",
        {"name": "RenderCam", "location": {"x": 6.0, "y": -6.0, "z": 4.0}, "set_active": True},
    )
    call("camera.auto_frame", {"camera": camera["camera"]["id"]})

    settings = call("render.settings.get")
    check(
        "render.settings.get",
        settings is not None and len(settings["available_engines"]) > 0,
        str(settings)[:200] if settings else None,
    )

    # Which engines exist genuinely varies: Blender 5.x dropped Workbench, and
    # Cycles is only present when its add-on is enabled. Drive the test from
    # what the build reports rather than from an assumption.
    available = settings["available_engines"] if settings else []
    if any(name.startswith("BLENDER_EEVEE") for name in available):
        engine = call("render.engine.set", {"engine": "EEVEE"})
        check(
            "render.engine.set resolves the build-specific identifier",
            engine is not None and engine["engine"].startswith("BLENDER_EEVEE"),
            str(engine),
        )
    error = call("render.engine.set", {"engine": "RENDERMAN"}, expect_error="INVALID_ENUM")
    check(
        "unknown engines are refused with the accepted set",
        error is not None and "allowed" in error.get("details", {}),
        str(error),
    )
    if "BLENDER_WORKBENCH" not in available:
        unavailable = call(
            "render.engine.set", {"engine": "WORKBENCH"}, expect_error="CAPABILITY_UNAVAILABLE"
        )
        check(
            "an engine this build lacks is refused with the real list",
            unavailable is not None and "available" in unavailable.get("details", {}),
            str(unavailable),
        )

    updated = call(
        "render.settings.update",
        {"resolution_x": 160, "resolution_y": 120, "format": "PNG", "transparent_background": True},
    )
    check("render.settings.update", updated is not None and "resolution_x" in updated["changed"])

    with tempfile.TemporaryDirectory(prefix="blender-mcp-render-") as directory:
        target = os.path.join(directory, "still.png")
        rendered = call("render.execute", {"output_path": target, "scope": {"frame": 1}})
        check(
            "render.execute wrote a file",
            rendered is not None and rendered["files"][0]["size_bytes"] > 0,
            str(rendered)[:300] if rendered else None,
        )
        check(
            "render reports its dimensions",
            rendered is not None and rendered["width"] == 160 and rendered["height"] == 120,
            str(rendered)[:200] if rendered else None,
        )

        # A per-call resolution is restored afterwards, so the reported size has
        # to be captured before the restore or it describes the old settings
        # rather than the file that was written.
        one_off = os.path.join(directory, "one_off.png")
        overridden = call(
            "render.execute",
            {"output_path": one_off, "scope": {"frame": 1}, "resolution_x": 200, "resolution_y": 100},
        )
        header = open(one_off, "rb").read(24)
        actual = (int.from_bytes(header[16:20], "big"), int.from_bytes(header[20:24], "big"))
        check(
            "a one-off resolution is reported as rendered, not as restored",
            overridden is not None
            and (overridden["width"], overridden["height"]) == (200, 100) == actual,
            f"reported {overridden['width']}x{overridden['height']}, file {actual[0]}x{actual[1]}"
            if overridden
            else None,
        )
        restored = call("render.settings.get")
        check(
            "the scene keeps its own resolution",
            restored is not None and restored["settings"]["resolution_x"] == 160,
            str(restored)[:200] if restored else None,
        )

        # A relative path is refused: the server owns path construction.
        call(
            "render.execute",
            {"output_path": "relative.png"},
            expect_error="INVALID_PATH",
        )

        # A frame range produces one numbered file per frame.
        sequence_target = os.path.join(directory, "seq.png")
        sequence = call(
            "render.execute",
            {"output_path": sequence_target, "scope": {"range": {"start": 1, "end": 3, "step": 1}}},
        )
        check(
            "render.execute renders a range",
            sequence is not None and len(sequence["files"]) == 3,
            str(sequence)[:200] if sequence else None,
        )
        check(
            "range frames are numbered distinctly",
            sequence is not None
            and len({entry["path"] for entry in sequence["files"]}) == 3,
            str(sequence["files"] if sequence else None),
        )

    # Headless Blender cannot capture a viewport, and says so.
    call(
        "render.viewport_screenshot",
        {"output_path": os.path.join(tempfile.gettempdir(), "shot.png")},
        expect_error="UNSUPPORTED_OPERATION",
    )


def test_animation() -> None:
    reset_scene()
    cube = call("object.create", {"type": "CUBE", "name": "Animated"})
    cube_id = cube["object"]["id"] if cube else None

    call("animation.range.set", {"frame_start": 1, "frame_end": 120})
    frame_range = call("animation.range.get")
    check("animation.range", frame_range is not None and frame_range["frame_end"] == 120)

    call("animation.range.set", {"frame_start": 200, "frame_end": 100}, expect_error="INVALID_ARGUMENT")
    # The failed range must not have been half-applied.
    frame_range = call("animation.range.get")
    check(
        "a rejected range leaves the scene alone",
        frame_range is not None and frame_range["frame_end"] == 120,
        str(frame_range),
    )

    inserted = call(
        "animation.keyframe.insert",
        {
            "object": cube_id,
            "target": {"location": {}},
            "keyframes": [
                {"frame": 1, "value": {"vector": {"x": 0, "y": 0, "z": 0}}, "interpolation": "LINEAR"},
                {"frame": 60, "value": {"vector": {"x": 5, "y": 0, "z": 0}}, "interpolation": "BEZIER"},
            ],
        },
    )
    check("animation.keyframe.insert", inserted is not None and inserted["inserted"] == 2, str(inserted))
    check(
        "insert reports the action",
        inserted is not None and inserted["action"]["keyframe_count"] >= 6,
        str(inserted["action"] if inserted else None),
    )

    listed = call("animation.keyframe.list", {"object": cube_id, "target": {"location": {}}})
    check("animation.keyframe.list", listed is not None and listed["total"] == 6, str(listed)[:200])
    interpolations = {entry["interpolation"] for entry in listed["keyframes"]} if listed else set()
    check(
        "per-keyframe interpolation is honoured",
        interpolations == {"LINEAR", "BEZIER"},
        str(interpolations),
    )

    curves = call("animation.fcurve.list", {"object": cube_id})
    check("animation.fcurve.list", curves is not None and curves["total"] == 3, str(curves)[:200])

    curve = call("animation.fcurve.get", {"object": cube_id, "data_path": "location", "array_index": 0})
    check(
        "animation.fcurve.get",
        curve is not None and len(curve["keyframes"]) == 2 and curve["keyframes"][1]["value"] == 5.0,
        str(curve)[:200] if curve else None,
    )

    call(
        "animation.fcurve.update",
        {"object": cube_id, "data_path": "location", "array_index": 0, "cyclic": True},
    )
    curve = call("animation.fcurve.get", {"object": cube_id, "data_path": "location", "array_index": 0})
    check(
        "cyclic modifier added",
        curve is not None and "CYCLES" in curve["fcurve"]["modifiers"],
        str(curve["fcurve"] if curve else None),
    )

    call(
        "animation.interpolation.set",
        {"object": cube_id, "target": {"location": {}}, "interpolation": "CONSTANT"},
    )
    listed = call("animation.keyframe.list", {"object": cube_id, "target": {"location": {}}})
    check(
        "animation.interpolation.set",
        listed is not None
        and all(entry["interpolation"] == "CONSTANT" for entry in listed["keyframes"]),
        str(listed)[:200] if listed else None,
    )

    removed = call(
        "animation.keyframe.delete",
        {"object": cube_id, "target": {"location": {}}, "frames": [60]},
    )
    check("animation.keyframe.delete", removed is not None and removed["removed"] == 3, str(removed))

    # Generated motion.
    reset_scene()
    turntable = call("object.create", {"type": "MONKEY", "name": "Spin"})
    spin_id = turntable["object"]["id"] if turntable else None
    rotation = call(
        "animation.create_rotation",
        {
            "object": spin_id,
            "start_frame": 1,
            "end_frame": 120,
            "axis": "Z",
            "degrees": 360.0,
            "loop_forever": True,
        },
    )
    check("animation.create_rotation", rotation is not None and rotation["inserted"] == 2, str(rotation))
    keys = call("animation.keyframe.list", {"object": spin_id, "target": {"rotation_euler": {}}})
    z_values = (
        sorted(entry["value"] for entry in keys["keyframes"] if entry["array_index"] == 2)
        if keys
        else []
    )
    check(
        "a full turn is 2 pi radians",
        len(z_values) == 2 and abs(z_values[1] - z_values[0] - 2 * math.pi) < 1e-6,
        str(z_values),
    )

    call(
        "animation.create_rotation",
        {"object": spin_id, "start_frame": 1, "end_frame": 1, "degrees": 90},
        expect_error="INVALID_ARGUMENT",
    )

    moved = call(
        "animation.create_move",
        {"object": spin_id, "start_frame": 1, "end_frame": 50, "by": {"x": 3, "y": 0, "z": 0}},
    )
    check("animation.create_move", moved is not None and moved["inserted"] == 2, str(moved))
    call(
        "animation.create_move",
        {"object": spin_id, "start_frame": 1, "end_frame": 50, "by": {"x": 1, "y": 0, "z": 0},
         "to": {"x": 1, "y": 0, "z": 0}},
        expect_error="INVALID_ARGUMENT",
    )

    scaled = call(
        "animation.create_scale",
        {"object": spin_id, "start_frame": 1, "end_frame": 25, "to": {"x": 2, "y": 2, "z": 2}},
    )
    check("animation.create_scale", scaled is not None and scaled["inserted"] == 2, str(scaled))
    call(
        "animation.create_scale",
        {"object": spin_id, "start_frame": 1, "end_frame": 25, "to": {"x": 0, "y": 1, "z": 1}},
        expect_error="INVALID_ARGUMENT",
    )

    # Actions and NLA.
    action = call("animation.action.create", {"name": "Walk"})
    check("animation.action.create", action is not None and action["action"]["name"] == "Walk")
    action_id = action["action"]["id"] if action else None

    assigned = call("animation.action.assign", {"object": spin_id, "action": action_id})
    check("animation.action.assign", assigned is not None, str(assigned))

    call("animation.nla.track.create", {"object": spin_id, "name": "Base"})
    strip = call(
        "animation.nla.strip.create",
        {"object": spin_id, "track": "Base", "action": action_id, "start_frame": 1,
         "blend_type": "REPLACE", "repeat": 2.0},
    )
    check("animation.nla.strip.create", strip is not None and strip["strip"]["repeat"] == 2.0, str(strip))

    tracks = call("animation.nla.track.list", {"object": spin_id})
    check(
        "animation.nla.track.list",
        tracks is not None and tracks["total"] == 1 and len(tracks["tracks"][0]["strips"]) == 1,
        str(tracks)[:250] if tracks else None,
    )

    call("animation.nla.strip.delete", {"object": spin_id, "track": "Base", "strip": strip["strip"]["name"]})
    call("animation.nla.track.delete", {"object": spin_id, "track": "Base"})
    tracks = call("animation.nla.track.list", {"object": spin_id})
    check("nla teardown", tracks is not None and tracks["total"] == 0)

    # A data path that is not a data path is refused.
    call(
        "animation.keyframe.insert",
        {
            "object": spin_id,
            "target": {"data_path": {"path": "location; __import__('os').system('x')"}},
            "keyframes": [{"frame": 1}],
        },
        expect_error="INVALID_PROPERTY",
    )

    # A real data path works.
    ok = call(
        "animation.keyframe.insert",
        {
            "object": spin_id,
            "target": {"data_path": {"path": "location", "index": 2}},
            "keyframes": [{"frame": 10, "value": {"scalar": 4.0}}],
        },
    )
    check("explicit data paths still work", ok is not None and ok["inserted"] == 1, str(ok))


def test_uv_and_images() -> None:
    reset_scene()
    cube = call("object.create", {"type": "CUBE", "name": "Unwrapped"})
    cube_id = cube["object"]["id"] if cube else None

    maps = call("uv.maps.list", {"object": cube_id})
    check("uv.maps.list on a fresh cube", maps is not None and maps["total"] == 1, str(maps))

    created = call("uv.map.create", {"object": cube_id, "name": "Lightmap"})
    check("uv.map.create", created is not None and created["uv_map"] == "Lightmap")
    call("uv.map.create", {"object": cube_id, "name": "Lightmap"}, expect_error="INVALID_ARGUMENT")

    call("uv.map.set_active", {"object": cube_id, "name": "UVMap"})
    unwrapped = call("uv.unwrap.angle_based", {"object": cube_id, "margin": 0.02})
    check("uv.unwrap.angle_based", unwrapped is not None and unwrapped["faces"] == 6, str(unwrapped))

    # Degrees, because that is what the protocol states -- a value that is only
    # legal as radians hides the conversion going missing. `margin` and the two
    # flags are named differently on this operator than on `unwrap`, so passing
    # them is what proves the mapping is right.
    smart = call(
        "uv.smart_project",
        {
            "object": cube_id,
            "angle_limit": 66,
            "margin": 0.02,
            "correct_aspect": True,
            "scale_to_bounds": False,
        },
    )
    check("uv.smart_project", smart is not None and smart["faces"] == 6, str(smart))

    cube_projected = call("uv.cube_project", {"object": cube_id, "projection_size": 2.0})
    check("uv.cube_project", cube_projected is not None, str(cube_projected))

    seamed = call(
        "uv.mark_seam",
        {"object": cube_id, "selection": {"type": "EDGE", "indices": [0, 1, 2, 3]}},
    )
    check("uv.mark_seam", seamed is not None and seamed["edges"] == 4, str(seamed))
    cleared = call(
        "uv.clear_seam",
        {"object": cube_id, "selection": {"type": "EDGE", "indices": [0, 1]}},
    )
    check("uv.clear_seam", cleared is not None and cleared["seam"] is False, str(cleared))
    call(
        "uv.mark_seam",
        {"object": cube_id, "selection": {"type": "FACE", "indices": [0]}},
        expect_error="INVALID_ARGUMENT",
    )

    packed = call("uv.pack_islands", {"objects": [cube_id], "margin": 0.01})
    check("uv.pack_islands", packed is not None, str(packed))

    averaged = call("uv.average_island_scale", {"object": cube_id})
    check("uv.average_island_scale", averaged is not None, str(averaged))

    call("uv.map.delete", {"object": cube_id, "name": "Lightmap"})
    maps = call("uv.maps.list", {"object": cube_id})
    check("uv.map.delete", maps is not None and maps["total"] == 1)

    # Headless builds cannot project from view.
    call("uv.project_from_view", {"object": cube_id}, expect_error="UNSUPPORTED_OPERATION")

    # Images.
    listed = call("image.list", {})
    check("image.list on an empty file", listed is not None and listed["total"] == 0, str(listed))

    call(
        "image.load",
        {"source_path": "relative/path.png"},
        expect_error="INVALID_PATH",
    )
    call(
        "image.load",
        {"source_path": os.path.join(tempfile.gettempdir(), "definitely-not-here.png")},
        expect_error="INVALID_PATH",
    )

    # Baking needs Cycles, which --factory-startup does not provide. The error
    # must say that rather than failing obscurely.
    call(
        "texture.bake",
        {
            "target": cube_id,
            "type": "NORMAL",
            "output_path": os.path.join(tempfile.gettempdir(), "bake.png"),
        },
        expect_error="CAPABILITY_UNAVAILABLE",
    )


def test_import_export() -> None:
    reset_scene()
    call("object.create", {"type": "MONKEY", "name": "Suzanne"})
    call("object.create", {"type": "CUBE", "name": "Box"})

    caps = call("io.capabilities")
    check(
        "io.capabilities lists real formats",
        caps is not None and len(caps["export"]) > 0,
        str(caps)[:300] if caps else None,
    )
    export_formats = {entry["format"] for entry in caps["export"]} if caps else set()

    with tempfile.TemporaryDirectory(prefix="blender-mcp-io-") as directory:
        # OBJ is present in every 4.x and 5.x build.
        if "OBJ" in export_formats:
            target = os.path.join(directory, "scene.obj")
            exported = call(
                "io.export",
                {
                    "destination_path": target,
                    "format": "OBJ",
                    "selection": {"scene": {}},
                    "apply_modifiers": True,
                },
            )
            check(
                "io.export wrote a file",
                exported is not None and exported["size_bytes"] > 0,
                str(exported)[:200] if exported else None,
            )

            reset_scene()
            imported = call("io.import", {"source_path": target, "format": "OBJ"})
            check(
                "io.import brought objects back",
                imported is not None and imported["count"] >= 2,
                str(imported)[:200] if imported else None,
            )

            # Selection export with an explicit object list.
            first = imported["imported"][0]["id"] if imported else None
            single = os.path.join(directory, "one.obj")
            partial = call(
                "io.export",
                {
                    "destination_path": single,
                    "format": "OBJ",
                    "selection": {"objects": [first]},
                },
            )
            check("io.export with an object list", partial is not None and partial["objects"] == 1)

        # Relative paths are refused on both sides.
        call(
            "io.export",
            {"destination_path": "out.obj", "format": "OBJ"},
            expect_error="INVALID_PATH",
        )
        call(
            "io.import",
            {"source_path": "in.obj", "format": "OBJ"},
            expect_error="INVALID_PATH",
        )

        # A format this build has no operator for is reported as unsupported.
        missing = call(
            "io.export",
            {"destination_path": os.path.join(directory, "x.svg"), "format": "SVG"},
            expect_error="UNSUPPORTED_FORMAT",
        )
        check(
            "unsupported formats say what was tried",
            missing is not None and "tried" in missing.get("details", {}),
            str(missing),
        )

        # Saving the .blend file.
        blend = os.path.join(directory, "scene.blend")
        saved = call("file.save", {"destination_path": blend})
        check("file.save", saved is not None and saved["size_bytes"] > 0, str(saved))
        info = call("file.info")
        check("file.info", info is not None and info["is_saved"] is True, str(info))


def test_rigging() -> None:
    reset_scene()
    rig = call(
        "rig.armature.create",
        {
            "name": "Rig",
            "bones": [
                {"name": "Spine", "head": {"x": 0, "y": 0, "z": 0}, "tail": {"x": 0, "y": 0, "z": 1}},
                {
                    "name": "Arm.L",
                    "head": {"x": 0.2, "y": 0, "z": 1},
                    "tail": {"x": 1.0, "y": 0, "z": 1},
                    "parent": "Spine",
                },
                {
                    "name": "Arm.R",
                    "head": {"x": -0.2, "y": 0, "z": 1},
                    "tail": {"x": -1.0, "y": 0, "z": 1},
                    "parent": "Spine",
                },
            ],
        },
    )
    check("rig.armature.create", rig is not None and rig["armature"]["bone_count"] == 3, str(rig)[:200])
    rig_id = rig["armature"]["id"] if rig else None

    # Parents must be declared before children.
    call(
        "rig.armature.create",
        {
            "name": "Bad",
            "bones": [
                {"name": "Child", "head": {"x": 0, "y": 0, "z": 0}, "tail": {"x": 0, "y": 0, "z": 1},
                 "parent": "Missing"},
            ],
        },
        expect_error="INVALID_ARGUMENT",
    )

    # Zero-length bones are refused rather than silently discarded by Blender.
    call(
        "rig.bone.create",
        {"armature": rig_id, "name": "Zero", "head": {"x": 0, "y": 0, "z": 0},
         "tail": {"x": 0, "y": 0, "z": 0}},
        expect_error="INVALID_ARGUMENT",
    )

    bones = call("rig.bone.list", {"armature": rig_id})
    check("rig.bone.list", bones is not None and bones["total"] == 3, str(bones)[:200])

    bone = call("rig.bone.get", {"armature": rig_id, "bone": "Arm.L"})
    check("rig.bone.get", bone is not None and bone["bone"]["parent"] == "Spine", str(bone)[:200])
    check("bones carry stable ids", bone is not None and len(bone["bone"]["id"]) == 36, str(bone)[:120])

    call("rig.bone.parent", {"armature": rig_id, "bone": "Spine", "parent": "Arm.L"},
         expect_error="INVALID_ARGUMENT")

    created = call("rig.bone.create", {
        "armature": rig_id, "name": "Head", "head": {"x": 0, "y": 0, "z": 1},
        "tail": {"x": 0, "y": 0, "z": 1.4}, "parent": "Spine", "connected": True,
    })
    check("rig.bone.create", created is not None and created["bone"]["connected"] is True, str(created)[:200])

    # Mirroring, dry run first.
    planned = call(
        "rig.bone.mirror",
        {"armature": rig_id, "bones": ["Arm.L"], "direction": "LEFT_TO_RIGHT", "dry_run": True},
    )
    check(
        "rig.bone.mirror dry run reports the existing pair",
        planned is not None and planned["planned"][0]["skipped"] is not None,
        str(planned),
    )

    # Vertex groups and binding.
    mesh = call("object.create", {"type": "CUBE", "name": "Body", "dimensions": {"x": 1, "y": 1, "z": 2}})
    mesh_id = mesh["object"]["id"] if mesh else None

    bound = call("rig.parent_mesh", {"armature": rig_id, "meshes": [mesh_id]})
    check(
        "rig.parent_mesh created vertex groups",
        bound is not None and bound["meshes"][0]["vertex_groups"] > 0,
        str(bound)[:200] if bound else None,
    )

    groups = call("rig.vertex_group.list", {"object": mesh_id})
    check("rig.vertex_group.list", groups is not None and groups["total"] > 0, str(groups)[:200])

    call("rig.vertex_group.create", {"object": mesh_id, "group": "Extra"})
    assigned = call(
        "rig.vertex_group.assign",
        {"object": mesh_id, "group": "Extra", "vertices": [0, 1], "weight": 0.5},
    )
    check("rig.vertex_group.assign", assigned is not None and assigned["vertices"] == 2, str(assigned))
    call(
        "rig.vertex_group.assign",
        {"object": mesh_id, "group": "Extra", "vertices": [9999]},
        expect_error="INVALID_ARGUMENT",
    )

    normalized = call(
        "rig.vertex_group.normalize", {"objects": [mesh_id], "dry_run": True}
    )
    check(
        "rig.vertex_group.normalize dry run",
        normalized is not None and normalized["dry_run"] is True,
        str(normalized)[:200],
    )

    # Constraints.
    added = call(
        "rig.constraint.add",
        {"object": rig_id, "bone": "Arm.L", "type": "COPY_ROTATION", "target": rig_id,
         "subtarget": "Spine", "influence": 0.5},
    )
    check("rig.constraint.add", added is not None and added["constraint"]["type"] == "COPY_ROTATION")

    constraints = call("rig.constraint.list", {"object": rig_id, "bone": "Arm.L"})
    check("rig.constraint.list", constraints is not None and constraints["total"] == 1, str(constraints)[:200])

    call(
        "rig.constraint.update",
        {"object": rig_id, "bone": "Arm.L", "constraint": added["constraint"]["name"],
         "properties": [{"name": "not_real", "value": {"float": 1.0}}]},
        expect_error="INVALID_PROPERTY",
    )

    call("rig.constraint.remove",
         {"object": rig_id, "bone": "Arm.L", "constraint": added["constraint"]["name"]})
    constraints = call("rig.constraint.list", {"object": rig_id, "bone": "Arm.L"})
    check("rig.constraint.remove", constraints is not None and constraints["total"] == 0)

    call("rig.bone.get", {"armature": rig_id, "bone": "Nope"}, expect_error="BONE_NOT_FOUND")
    call("rig.armature.get", {"armature": mesh_id}, expect_error="ARMATURE_NOT_FOUND")


def test_rig_diagnostics() -> None:
    reset_scene()
    rig = call(
        "rig.armature.create",
        {
            "name": "Rig",
            "bones": [
                {"name": "Spine", "head": {"x": 0, "y": 0, "z": 0}, "tail": {"x": 0, "y": 0, "z": 1}},
                {"name": "Arm_L", "head": {"x": 0.2, "y": 0, "z": 1}, "tail": {"x": 1.0, "y": 0, "z": 1},
                 "parent": "Spine"},
                {"name": "Arm_R", "head": {"x": -0.2, "y": 0, "z": 1}, "tail": {"x": -1.2, "y": 0, "z": 1},
                 "parent": "Spine"},
            ],
        },
    )
    rig_id = rig["armature"]["id"] if rig else None

    health = call("rig.diagnostics.health", {"armature": rig_id})
    check("rig.diagnostics.health", health is not None, str(health)[:200])
    codes = {f["code"] for f in health["findings"]} if health else set()
    check(
        "an unbound rig is reported",
        "NO_BOUND_MESHES" in codes,
        str(codes),
    )

    naming = call("rig.diagnostics.naming", {"armature": rig_id})
    check(
        "rig.diagnostics.naming detects the convention",
        naming is not None and naming["convention"] == "UNDERSCORE_SUFFIX",
        str(naming)[:200] if naming else None,
    )

    sym = call("rig.diagnostics.symmetry", {"armature": rig_id})
    codes = {f["code"] for f in sym["findings"]} if sym else set()
    check(
        "asymmetric bones are found",
        "ASYMMETRIC_BONE" in codes or "ASYMMETRIC_LENGTH" in codes,
        str(sym)[:300] if sym else None,
    )
    check("symmetry compared the pair", sym is not None and sym["pairs_compared"] == 1, str(sym)[:150])

    # Renaming to Blender convention, dry run then applied.
    planned = call("rig.fix.naming", {"armature": rig_id, "convention": "DOT_SUFFIX", "dry_run": True})
    check(
        "rig.fix.naming dry run proposes renames",
        planned is not None and len(planned["renames"]) == 2 and not planned["renames"][0]["applied"],
        str(planned)[:250] if planned else None,
    )

    applied = call("rig.fix.naming", {"armature": rig_id, "convention": "DOT_SUFFIX", "dry_run": False})
    check(
        "rig.fix.naming applies",
        applied is not None and all(entry["applied"] for entry in applied["renames"]),
        str(applied)[:250] if applied else None,
    )
    bones = call("rig.bone.list", {"armature": rig_id})
    names = {bone["name"] for bone in bones["bones"]} if bones else set()
    check("bones were renamed to the Blender convention", {"Arm.L", "Arm.R"} <= names, str(names))

    # Weights on an unbound mesh.
    mesh = call("object.create", {"type": "CUBE", "name": "Body"})
    weights = call("rig.diagnostics.weights", {"objects": [mesh["object"]["id"]]})
    codes = {f["code"] for f in weights["findings"]} if weights else set()
    check("a mesh with no groups is reported", "NO_VERTEX_GROUPS" in codes, str(codes))

    call("rig.parent_mesh", {"armature": rig_id, "meshes": [mesh["object"]["id"]]})
    weights = call(
        "rig.diagnostics.weights",
        {"objects": [mesh["object"]["id"]], "max_influences": 4},
    )
    check("weights diagnostics run on a bound mesh", weights is not None, str(weights)[:200])
    check(
        "weight statistics are reported",
        weights is not None and weights["objects"][0]["vertex_groups"] > 0,
        str(weights["objects"] if weights else None),
    )

    # Scene-level hunts.
    duplicates = call("scene.find_duplicates", {})
    check("scene.find_duplicates", duplicates is not None, str(duplicates)[:200])
    missing = call("scene.find_missing_textures", {})
    check("scene.find_missing_textures", missing is not None and missing["total"] == 0, str(missing))


def test_geometry_nodes() -> None:
    reset_scene()
    ground = call("object.create", {"type": "PLANE", "name": "Ground", "dimensions": {"x": 10, "y": 10, "z": 0}})
    ground_id = ground["object"]["id"] if ground else None

    group = call("geometry_nodes.group.create", {"name": "Scatter", "attach_to": ground_id})
    check(
        "geometry_nodes.group.create",
        group is not None and group["attached"] is not None,
        str(group)[:250] if group else None,
    )
    group_id = group["group"]["id"] if group else None
    check(
        "a new group has geometry in and out",
        group is not None and len(group["group"]["inputs"]) == 1 and len(group["group"]["outputs"]) == 1,
        str(group["group"] if group else None)[:250],
    )

    listed = call("geometry_nodes.group.list", {})
    check("geometry_nodes.group.list", listed is not None and listed["total"] == 1)

    socket = call(
        "geometry_nodes.interface.add_socket",
        {"group": group_id, "name": "Density", "type": "FLOAT", "min": 0.0, "max": 100.0,
         "default_value": {"float": 10.0}},
    )
    check("geometry_nodes.interface.add_socket", socket is not None, str(socket))

    interface = call("geometry_nodes.interface.list", {"group": group_id})
    density = (
        next((s for s in interface["inputs"] if s["name"] == "Density"), None) if interface else None
    )
    check("interface socket round trip", density is not None, str(interface)[:250] if interface else None)
    check(
        "interface bounds are stored",
        density is not None and abs(density.get("max", 0) - 100.0) < 1e-6,
        str(density),
    )

    call(
        "geometry_nodes.interface.update_socket",
        {"group": group_id, "socket": "Density", "name": "Instances"},
    )
    interface = call("geometry_nodes.interface.list", {"group": group_id})
    check(
        "interface socket renamed",
        interface is not None and any(s["name"] == "Instances" for s in interface["inputs"]),
        str(interface)[:200] if interface else None,
    )

    call(
        "geometry_nodes.interface.delete_socket",
        {"group": group_id, "socket": "NotThere"},
        expect_error="INVALID_NODE_SOCKET",
    )

    # A declarative graph plan, which is what the Rust workflow layer sends.
    built = call(
        "geometry_nodes.graph.build",
        {
            "node_tree": group_id,
            "clear": True,
            "nodes": [
                {"key": "in", "node_type": "NodeGroupInput", "location": {"x": -600, "y": 0}},
                {"key": "distribute", "node_type": "GeometryNodeDistributePointsOnFaces",
                 "location": {"x": -300, "y": 0},
                 "inputs": [{"name": "Density", "value": {"float": 5.0}}]},
                {"key": "instance", "node_type": "GeometryNodeInstanceOnPoints",
                 "location": {"x": 0, "y": 0}},
                {"key": "out", "node_type": "NodeGroupOutput", "location": {"x": 300, "y": 0}},
            ],
            "links": [
                {"from": {"node": "in", "index": 0}, "to": {"node": "distribute", "name": "Mesh"}},
                {"from": {"node": "distribute", "name": "Points"},
                 "to": {"node": "instance", "name": "Points"}},
                {"from": {"node": "instance", "name": "Instances"},
                 "to": {"node": "out", "index": 0}},
            ],
        },
    )
    check(
        "geometry_nodes.graph.build",
        built is not None and len(built["nodes"]) == 4 and len(built["links"]) == 3,
        str(built)[:300] if built else None,
    )

    tree = call("geometry_nodes.tree.get", {"node_tree": group_id})
    check(
        "the built graph reads back",
        tree is not None and len(tree["tree"]["nodes"]) == 4,
        str(tree)[:200] if tree else None,
    )

    # A node type that does not exist is refused.
    call(
        "geometry_nodes.node.create",
        {"node_tree": group_id, "node_type": "GeometryNodeNotReal"},
        expect_error="INVALID_NODE_TYPE",
    )

    modifiers = call("geometry_nodes.modifier.list", {"object": ground_id})
    check("geometry_nodes.modifier.list", modifiers is not None and modifiers["total"] == 1)

    call("geometry_nodes.modifier.detach", {"object": ground_id})
    modifiers = call("geometry_nodes.modifier.list", {"object": ground_id})
    check("geometry_nodes.modifier.detach", modifiers is not None and modifiers["total"] == 0)

    attached = call("geometry_nodes.modifier.attach", {"object": ground_id, "group": group_id})
    check("geometry_nodes.modifier.attach", attached is not None, str(attached)[:200])

    call("geometry_nodes.group.delete", {"group": group_id}, expect_error="INVALID_ARGUMENT")
    call("geometry_nodes.modifier.detach", {"object": ground_id})
    call("geometry_nodes.group.delete", {"group": group_id})
    listed = call("geometry_nodes.group.list", {})
    check("geometry_nodes.group.delete", listed is not None and listed["total"] == 0)


def test_utilities() -> None:
    reset_scene()
    call("object.create", {"type": "CUBE", "name": "KeepMe"})
    call("object.create", {"type": "CUBE", "name": "AlsoKeep"})
    call("collection.create", {"name": "Empty"})
    call("material.create", {"name": "Wood"})
    call("material.create", {"name": "Wood.001"})

    # Cleanup does nothing unless asked.
    call("scene.cleanup", {}, expect_error="INVALID_ARGUMENT")

    dry = call(
        "scene.cleanup",
        {"remove_empty_collections": True, "merge_duplicate_materials": True, "dry_run": True},
    )
    check(
        "scene.cleanup dry run reports without changing",
        dry is not None and dry["passes"]["remove_empty_collections"]["count"] == 1,
        str(dry)[:250] if dry else None,
    )
    collections = call("collection.list", {})
    check(
        "a dry run changed nothing",
        collections is not None and collections["total"] == 1,
        str(collections)[:150],
    )

    applied = call(
        "scene.cleanup",
        {"remove_empty_collections": True, "merge_duplicate_materials": True, "purge_orphans": True},
    )
    check("scene.cleanup applied", applied is not None and applied["dry_run"] is False)
    collections = call("collection.list", {})
    check("empty collection removed", collections is not None and collections["total"] == 0)
    check(
        "duplicate materials merged",
        applied is not None and applied["passes"]["merge_duplicate_materials"]["count"] == 1,
        str(applied["passes"]["merge_duplicate_materials"] if applied else None),
    )
    # Both materials were unused, so the orphan purge in the same call removes
    # the survivor too -- which is the correct outcome, not a bug.
    materials = call("material.list", {})
    check(
        "unused materials were purged",
        materials is not None and materials["total"] == 0,
        str(materials)[:200] if materials else None,
    )

    # Batch rename, dry run then applied.
    planned = call(
        "scene.batch_rename",
        {"kind": "objects", "find": "Keep", "replace": "Prop", "dry_run": True},
    )
    check(
        "scene.batch_rename dry run",
        planned is not None and len(planned["renames"]) == 2
        and not planned["renames"][0]["applied"],
        str(planned)[:250] if planned else None,
    )

    renamed = call(
        "scene.batch_rename",
        {"kind": "objects", "prefix": "SM_", "number_start": 1, "number_padding": 2},
    )
    check(
        "scene.batch_rename applied",
        renamed is not None and all(entry["applied"] for entry in renamed["renames"]),
        str(renamed)[:250] if renamed else None,
    )
    objects = call("object.list", {})
    names = {o["name"] for o in objects["objects"]} if objects else set()
    check("names carry the prefix and number", any(n.startswith("SM_") for n in names), str(names))

    call(
        "scene.batch_rename",
        {"kind": "objects", "regex": "([", "replace": "x"},
        expect_error="INVALID_ARGUMENT",
    )

    # Applying transforms.
    obj = call("object.create", {"type": "CUBE", "name": "Scaled", "scale": {"x": 2, "y": 2, "z": 2}})
    applied = call("scene.apply_transforms", {"objects": [obj["object"]["id"]], "scale": True})
    check("scene.apply_transforms", applied is not None and len(applied["applied"]) == 1, str(applied)[:200])
    fetched = call("object.get", {"object": obj["object"]["id"]})
    check(
        "scale is now identity",
        fetched is not None and abs(fetched["object"]["scale"]["x"] - 1.0) < 1e-6,
        str(fetched["object"]["scale"] if fetched else None),
    )

    call("scene.apply_transforms", {"objects": [obj["object"]["id"]]},
         expect_error="INVALID_ARGUMENT") if False else None

    analysis = call("scene.mesh_analysis", {})
    check(
        "scene.mesh_analysis covers every mesh",
        analysis is not None and analysis["total"] >= 3,
        str(analysis)[:200] if analysis else None,
    )

    purged = call("scene.purge_orphans", {"dry_run": True})
    check("scene.purge_orphans dry run", purged is not None and purged["dry_run"] is True, str(purged)[:200])


def test_pump_cadence() -> None:
    """The pump asks to be run again at a rate that matches the traffic.

    This is the whole of an operation's latency: a request cannot be answered
    before the next pump tick, so a wrong answer here is measurable in every
    round trip. The three branches are cheap to check and expensive to get
    wrong, so they are checked.
    """
    from blender_extension import config

    state = dispatcher.BridgeState()
    state.inbox = queue_module.inbox()

    now = 1000.0
    state.last_activity = 0.0
    check(
        "pump idles when nothing has happened",
        dispatcher.next_interval(state, now) == config.PUMP_INTERVAL_IDLE,
        str(dispatcher.next_interval(state, now)),
    )

    state.last_activity = now - config.PUMP_ACTIVE_WINDOW / 2
    check(
        "pump stays fast just after a request",
        dispatcher.next_interval(state, now) == config.PUMP_INTERVAL_BUSY,
        str(dispatcher.next_interval(state, now)),
    )

    state.last_activity = 0.0
    state.inbox.put({"queued": True}, timeout=0)
    check(
        "pump comes straight back when work is already queued",
        dispatcher.next_interval(state, now) == config.PUMP_INTERVAL_BUSY,
        str(dispatcher.next_interval(state, now)),
    )
    check(
        "the busy cadence is faster than the idle one",
        config.PUMP_INTERVAL_BUSY < config.PUMP_INTERVAL_IDLE,
        f"{config.PUMP_INTERVAL_BUSY} vs {config.PUMP_INTERVAL_IDLE}",
    )


def main() -> int:
    print(f"blender {bpy.app.version_string}, {operations.operation_count()} operations registered")
    reset_scene()

    # -- system ------------------------------------------------------------
    caps = call("system.capabilities")
    check("capabilities.engines", bool(caps and caps["capabilities"]["render_engines"]),
          "no render engines reported")
    check(
        "capabilities.shader_nodes",
        bool(caps and "ShaderNodeBsdfPrincipled" in caps["capabilities"]["shader_nodes"]),
        "Principled BSDF missing from the shader node list",
    )

    ops_list = call("system.operations")
    check("operations.listed", bool(ops_list and ops_list["count"] > 0))

    # -- scene -------------------------------------------------------------
    summary = call("scene.summary")
    check("scene.summary.empty", summary is not None and summary["objects"]["total"] == 0,
          f"expected an empty scene, got {summary['objects'] if summary else None}")

    # -- object creation ---------------------------------------------------
    created = call("object.create", {"type": "CUBE", "name": "Wall", "location": {"x": 1, "y": 2, "z": 3}})
    check("object.create.name", created is not None and created["object"]["name"] == "Wall")
    check(
        "object.create.location",
        created is not None and abs(created["object"]["location"]["x"] - 1.0) < 1e-6,
        "location was not applied",
    )
    cube_id = created["object"]["id"] if created else None

    # The id must survive a rename -- that is the whole point of having one.
    call("object.rename", {"object": cube_id, "name": "Renamed"})
    fetched = call("object.get", {"object": cube_id})
    check(
        "object.id.survives_rename",
        fetched is not None and fetched["object"]["name"] == "Renamed",
        "the id did not resolve after renaming",
    )

    # Dimensions.
    call("object.transform", {"object": cube_id, "dimensions": {"x": 4, "y": 0.2, "z": 3}})
    fetched = call("object.get", {"object": cube_id})
    dims = fetched["object"]["dimensions"] if fetched else {}
    check(
        "object.transform.dimensions",
        abs(dims.get("x", 0) - 4.0) < 1e-4 and abs(dims.get("z", 0) - 3.0) < 1e-4,
        f"dimensions came back as {dims}",
    )

    # Relative transforms.
    before = fetched["object"]["location"]["z"]
    call("object.transform", {"object": cube_id, "location": {"x": 0, "y": 0, "z": 5}, "relative": True})
    fetched = call("object.get", {"object": cube_id})
    check(
        "object.transform.relative",
        abs(fetched["object"]["location"]["z"] - (before + 5)) < 1e-6,
        "relative move did not add to the current location",
    )

    # Rotation in degrees.
    call("object.transform", {"object": cube_id, "rotation": {"degrees": {"x": 90, "y": 0, "z": 0}}})
    fetched = call("object.get", {"object": cube_id})
    import math

    check(
        "object.transform.degrees",
        abs(fetched["object"]["rotation_euler"]["x"] - math.pi / 2) < 1e-6,
        f"expected pi/2, got {fetched['object']['rotation_euler']}",
    )

    # -- primitives --------------------------------------------------------
    for primitive in ("PLANE", "UV_SPHERE", "ICO_SPHERE", "CYLINDER", "CONE", "TORUS", "MONKEY",
                      "EMPTY", "CURVE", "TEXT", "CAMERA", "LIGHT"):
        result = call("object.create", {"type": primitive, "name": f"P_{primitive}"})
        check(f"primitive.{primitive}", result is not None)

    # -- errors ------------------------------------------------------------
    call("object.get", {"object": "NoSuchObject"}, expect_error="OBJECT_NOT_FOUND")
    call("object.create", {"type": "SPHERE"}, expect_error="INVALID_ENUM")
    call("nonexistent.operation", {}, expect_error="UNSUPPORTED_OPERATION")
    call("object.transform", {"object": cube_id, "location": {"x": float("nan"), "y": 0, "z": 0}},
         expect_error="INVALID_TRANSFORM")

    # -- collections -------------------------------------------------------
    collection = call("collection.create", {"name": "Props", "objects": [cube_id]})
    check("collection.create", collection is not None and collection["collection"]["object_count"] == 1)
    collection_id = collection["collection"]["id"] if collection else None

    listed = call("collection.list", {})
    check("collection.list", listed is not None and listed["total"] >= 1)

    call("collection.set_visibility", {"collection": collection_id, "hide_render": True})
    fetched_collection = call("collection.get", {"collection": collection_id})
    check(
        "collection.visibility",
        fetched_collection is not None and fetched_collection["collection"]["hide_render"] is True,
    )

    # Unlinking an object's only collection must be refused, not silently done.
    call(
        "collection.unlink_object",
        {"collection": collection_id, "objects": [cube_id]},
        expect_error="INVALID_ARGUMENT",
    )

    # -- selection ---------------------------------------------------------
    call("selection.set", {"objects": [cube_id], "active": cube_id})
    selection = call("selection.get")
    check(
        "selection.set",
        selection is not None and selection["active"] == cube_id and cube_id in selection["selected"],
        f"selection came back as {selection}",
    )
    call("selection.clear")
    selection = call("selection.get")
    check("selection.clear", selection is not None and not selection["selected"])

    # -- listing and pagination -------------------------------------------
    listed = call("object.list", {"limit": 3})
    check("object.list.limit", listed is not None and len(listed["objects"]) == 3)
    check("object.list.cursor", listed is not None and listed["next_cursor"] == "3")
    page_two = call("object.list", {"limit": 3, "cursor": listed["next_cursor"]})
    check("object.list.page_two", page_two is not None and len(page_two["objects"]) > 0)
    first_page_ids = {o["id"] for o in listed["objects"]}
    second_page_ids = {o["id"] for o in page_two["objects"]}
    check(
        "object.list.pages_disjoint",
        not (first_page_ids & second_page_ids),
        "pagination returned overlapping pages",
    )

    filtered = call("object.list", {"types": ["CAMERA"]})
    check(
        "object.list.type_filter",
        filtered is not None and all(o["type"] == "CAMERA" for o in filtered["objects"]),
    )

    # -- duplication -------------------------------------------------------
    duplicated = call("object.duplicate", {"objects": [cube_id], "count": 2, "offset": {"x": 5, "y": 0, "z": 0}})
    check("object.duplicate.count", duplicated is not None and len(duplicated["objects"]) == 2)
    new_ids = {o["id"] for o in duplicated["objects"]} if duplicated else set()
    check(
        "object.duplicate.fresh_ids",
        cube_id not in new_ids and len(new_ids) == 2,
        "duplicates must not inherit the original's id",
    )

    # -- parenting ---------------------------------------------------------
    parent = call("object.create", {"type": "EMPTY", "name": "Rig"})
    parent_id = parent["object"]["id"] if parent else None
    call("object.set_parent", {"object": cube_id, "parent": parent_id})
    fetched = call("object.get", {"object": cube_id})
    check("object.set_parent", fetched is not None and fetched["object"].get("parent") == parent_id)
    # A cycle must be refused.
    call("object.set_parent", {"object": parent_id, "parent": cube_id}, expect_error="INVALID_ARGUMENT")

    # -- origin ------------------------------------------------------------
    # Every mode, because the three cursor-based ones go down a different code
    # path from the rest and a wrong enum there is invisible to the others.
    origin_cube = call("object.create", {"type": "CUBE", "name": "OriginProbe", "location": {"x": 0, "y": 0, "z": 5}, "options": {"size": 2}})
    origin_id = origin_cube["object"]["id"] if origin_cube else None
    for mode, expected in (
        ("ORIGIN_TO_BOUNDS_BOTTOM", (0.0, 0.0, 4.0)),
        ("ORIGIN_TO_BOUNDS_CENTER", (0.0, 0.0, 5.0)),
        ("ORIGIN_TO_GEOMETRY", (0.0, 0.0, 5.0)),
    ):
        moved = call("object.origin.set", {"objects": [origin_id], "mode": mode})
        loc = moved["objects"][0]["location"] if moved else None
        check(
            f"object.origin.set.{mode.lower()}",
            loc is not None and all(abs(loc[axis] - value) < 1e-5 for axis, value in zip("xyz", expected)),
            f"origin landed at {loc}, expected {expected}",
        )
    placed = call("object.origin.set", {"objects": [origin_id], "mode": "ORIGIN_TO_POINT", "point": {"x": 1, "y": 2, "z": 3}})
    loc = placed["objects"][0]["location"] if placed else None
    check(
        "object.origin.set.origin_to_point",
        loc is not None and all(abs(loc[axis] - value) < 1e-5 for axis, value in zip("xyz", (1.0, 2.0, 3.0))),
        f"origin landed at {loc}",
    )
    call("object.origin.set", {"objects": [origin_id], "mode": "ORIGIN_TO_POINT"}, expect_error="INVALID_ARGUMENT")
    call("object.delete", {"objects": [origin_id]})

    # -- events ------------------------------------------------------------
    # `created` and `deleted` events carry a field called `name`, which is also
    # the name of the positional parameter. Binding it as a keyword raised a
    # TypeError inside the depsgraph handler, so every object another add-on or
    # the user made went unreported.
    from blender_extension import protocol as _protocol

    payload = _protocol.event("session", 7, "created", kind="object", name="Cube")
    check(
        "event.name_field_does_not_shadow_the_event_name",
        payload["event"] == "created" and payload["name"] == "Cube",
        str(payload),
    )

    # -- statistics --------------------------------------------------------
    stats = call("scene.statistics")
    check("scene.statistics", stats is not None and stats["objects"]["total"] > 0)

    # -- world -------------------------------------------------------------
    call("scene.world.update", {"color": {"r": 0.05, "g": 0.05, "b": 0.08}, "strength": 1.5})
    world = call("scene.world.get")
    check(
        "scene.world.update",
        world is not None and world["world"] is not None and abs(world["world"]["strength"] - 1.5) < 1e-6,
    )

    # -- scene settings ----------------------------------------------------
    call("scene.settings.update", {"frame_start": 1, "frame_end": 120, "fps": 24})
    scene = call("scene.get")
    check(
        "scene.settings.update",
        scene is not None and scene["scene"]["frame_end"] == 120 and abs(scene["scene"]["fps"] - 24) < 1e-6,
    )

    # -- deletion ----------------------------------------------------------
    call("object.delete", {"objects": [cube_id]})
    call("object.get", {"object": cube_id}, expect_error="OBJECT_NOT_FOUND")

    # -- other domains -----------------------------------------------------
    test_materials_and_shaders()
    test_lights()
    test_modifiers()
    test_mesh()
    test_camera()
    test_render()
    test_animation()
    test_uv_and_images()
    test_import_export()
    test_rigging()
    test_rig_diagnostics()
    test_geometry_nodes()
    test_utilities()
    test_pump_cadence()

    print()
    print(f"passed: {PASSED}")
    if FAILED:
        print(f"failed: {len(FAILED)}")
        for name, detail in FAILED:
            print(f"  - {name}: {detail}")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    try:
        code = main()
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        code = 2
    # Blender ignores a plain return value, so the exit code has to be forced.
    sys.exit(code)

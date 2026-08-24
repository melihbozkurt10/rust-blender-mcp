"""Surface and opening queries, inside a real Blender.

    blender --background --factory-startup --python tests/blender/test_scene_surface.py

These are the operations scene-aware placement stands on, so they are tested
against real geometry rather than against a mock of it: a wall that is actually
rotated, a floor that is actually below it, a door that is actually an object.
The Rust side has its own tests over synthetic regions; this one checks that
what Blender hands over matches what those tests assume.
"""

from __future__ import annotations

import math
import os
import sys

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
sys.path.insert(0, REPO_ROOT)

import bpy  # noqa: E402

from blender_extension import dispatcher  # noqa: E402
from blender_extension import operations  # noqa: E402  (registers the handlers)

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


class Ctx:
    """What a handler expects besides its arguments."""

    revision = 1


def call(op: str, **args):
    return dispatcher.HANDLERS[op](Ctx(), args)


def vec(value) -> tuple[float, float, float]:
    return (value["x"], value["y"], value["z"])


def close(a: float, b: float, tolerance: float = 1e-4) -> bool:
    return abs(a - b) <= tolerance


def build_scene(yaw_degrees: float) -> None:
    """A facade, a floor and a door, turned as a whole by `yaw_degrees`.

    Everything is parented to one empty and the empty is what turns, which is
    how a real building arrives: nothing is axis-aligned and the object's own
    frame is the only thing that still makes sense.
    """
    bpy.ops.wm.read_factory_settings(use_empty=True)

    bpy.ops.object.empty_add(location=(0.0, 0.0, 0.0))
    site = bpy.context.object
    site.name = "site"

    # A 12m x 6m facade, 0.4m thick, standing on the ground and facing -Y.
    bpy.ops.mesh.primitive_cube_add(size=1.0)
    facade = bpy.context.object
    facade.name = "rear_facade"
    facade.scale = (12.0, 0.4, 6.0)
    facade.location = (0.0, 0.0, 3.0)
    facade.parent = site

    # A 20m x 20m floor at z = 0.
    bpy.ops.mesh.primitive_plane_add(size=20.0, location=(0.0, -6.0, 0.0))
    floor = bpy.context.object
    floor.name = "yard_floor"
    floor.parent = site

    # A 1.2m x 2.4m service door, sitting in the facade's outer face.
    #
    # Parented to the site rather than to the facade on purpose: the facade is
    # a stretched cube, and a child of it would inherit that stretch and come
    # out fourteen metres tall. Real scenes hit this; the test scene should
    # not pretend otherwise.
    bpy.ops.mesh.primitive_cube_add(size=1.0)
    door = bpy.context.object
    door.name = "service_door"
    door.scale = (1.2, 0.5, 2.4)
    door.location = (-2.0, -0.2, 1.2)
    door.parent = site

    site.rotation_euler = (0.0, 0.0, math.radians(yaw_degrees))
    bpy.context.view_layer.update()


def main() -> int:
    print(f"\n{len(dispatcher.HANDLERS)} bridge operations registered")

    # -- a plain wall -------------------------------------------------------
    build_scene(0.0)
    walls = call(
        "scene.surface.inspect",
        object="rear_facade",
        classification="WALL",
        min_area=4.0,
    )
    check(
        "a facade offers exactly its two large faces as walls",
        walls["total"] == 2,
        str([(r["classification"], round(r["area"], 2)) for r in walls["regions"]]),
    )
    outward = next(
        (r for r in walls["regions"] if vec(r["normal"])[1] < -0.9), None
    )
    check(
        "and one of them faces out, with the area it really has",
        outward is not None and close(outward["area"], 12.0 * 6.0, 0.01),
        str(outward and round(outward["area"], 3)),
    )
    if outward:
        check(
            "its extent is the wall's real width and height",
            close(outward["extent"]["along_max"] - outward["extent"]["along_min"], 12.0, 0.01)
            and close(
                outward["extent"]["across_max"] - outward["extent"]["across_min"], 6.0, 0.01
            ),
            str(outward["extent"]),
        )
        check(
            "and its tangent is horizontal, which is what left and right mean",
            close(vec(outward["tangent"])[2], 0.0),
            str(outward["tangent"]),
        )

    floors = call("scene.surface.inspect", object="yard_floor", classification="FLOOR")
    check(
        "a plane at z=0 is a floor at z=0",
        floors["total"] == 1 and close(vec(floors["regions"][0]["point"])[2], 0.0),
        str(floors["regions"][0]["point"] if floors["regions"] else None),
    )

    ceilings = call("scene.surface.inspect", object="yard_floor", classification="CEILING")
    check(
        "and it is not also a ceiling",
        ceilings["total"] == 0,
        str(ceilings["total"]),
    )

    # -- the same building, turned ------------------------------------------
    for yaw in (37.0, 90.0, -128.0):
        build_scene(yaw)
        turned = call(
            "scene.surface.inspect",
            object="rear_facade",
            classification="WALL",
            min_area=4.0,
        )
        found = None
        for region in turned["regions"]:
            normal = vec(region["normal"])
            # The outward face of a wall turned by `yaw` points at
            # (sin yaw, -cos yaw, 0).
            if close(normal[0], math.sin(math.radians(yaw)), 1e-3) and close(
                normal[1], -math.cos(math.radians(yaw)), 1e-3
            ):
                found = region
                break
        check(
            f"turned {yaw:g} degrees, the outward wall still resolves in world space",
            found is not None,
            str([{k: round(v, 3) for k, v in r["normal"].items()} for r in turned["regions"]]),
        )
        if found:
            check(
                f"turned {yaw:g} degrees, its area and extent are unchanged",
                close(found["area"], 72.0, 0.01)
                and close(
                    found["extent"]["along_max"] - found["extent"]["along_min"], 12.0, 0.01
                ),
                f"area {found['area']:.3f}",
            )
            check(
                f"turned {yaw:g} degrees, its own frame still says it faces -Y",
                close(vec(found["local_normal"])[1], -1.0, 1e-3),
                str(found["local_normal"]),
            )

    # -- openings -----------------------------------------------------------
    build_scene(37.0)
    none_yet = call("scene.openings.inspect", host="rear_facade")
    check(
        "an unmarked door is not an opening, and the answer says why",
        none_yet["total"] == 0 and "authored metadata" in none_yet["note"],
        none_yet["note"],
    )

    marked = call("scene.openings.mark", object="service_door", kind="SERVICE_DOOR", host="rear_facade")
    check(
        "marking one records the kind it was given",
        marked["kind"] == "SERVICE_DOOR" and marked["host"] == "rear_facade",
        str(marked),
    )

    openings = call("scene.openings.inspect", host="rear_facade")
    check(
        "and it then comes back for its host, in world space",
        openings["total"] == 1 and openings["openings"][0]["name"] == "service_door",
        str(openings["total"]),
    )
    if openings["total"] == 1:
        door = openings["openings"][0]
        centre = vec(door["centre"])
        check(
            "with the height the door really has",
            close(door["bounds"]["max"]["z"] - door["bounds"]["min"]["z"], 2.4, 0.01),
            str(door["bounds"]),
        )
        check(
            "and a centre that moved with the building",
            not close(centre[0], -2.0, 0.1),
            str(centre),
        )
        check(
            "reported as authored, never as inferred from geometry",
            door["source"] == "AUTHORED_METADATA",
            door["source"],
        )

    bad_kind = None
    try:
        call("scene.openings.mark", object="service_door", kind="PORTCULLIS")
    except Exception as error:  # the bridge's own typed refusal
        bad_kind = str(error)
    check(
        "a kind the domain does not have is refused",
        bad_kind is not None,
        str(bad_kind),
    )

    # -- raycast ------------------------------------------------------------
    hit = call(
        "scene.surface.raycast",
        objects=["yard_floor"],
        origin={"x": 0.0, "y": -6.0, "z": 40.0},
        direction={"x": 0.0, "y": 0.0, "z": -1.0},
    )
    check(
        "a ray dropped onto the yard finds the floor at z=0",
        hit["hit"] and close(vec(hit["result"]["point"])[2], 0.0, 1e-3),
        str(hit["result"]),
    )
    check(
        "and calls what it hit a floor",
        hit["hit"] and hit["result"]["classification"] == "FLOOR",
        str(hit["hit"] and hit["result"]["classification"]),
    )

    missed = call(
        "scene.surface.raycast",
        objects=["yard_floor"],
        origin={"x": 0.0, "y": -6.0, "z": 40.0},
        direction={"x": 0.0, "y": 0.0, "z": 1.0},
    )
    check(
        "a ray pointing away hits nothing, and says so rather than guessing",
        not missed["hit"] and missed["result"] is None,
        str(missed),
    )

    # -- the cache ----------------------------------------------------------
    first = call("scene.surface.inspect", object="rear_facade")
    second = call("scene.surface.inspect", object="rear_facade")
    check(
        "surfaces are derived once and reused",
        first["cached"] is False and second["cached"] is True,
        f"{first['cached']} then {second['cached']}",
    )

    facade = bpy.data.objects["rear_facade"]
    facade.location = (facade.location[0], facade.location[1], facade.location[2] + 1.0)
    bpy.context.view_layer.update()
    moved = call("scene.surface.inspect", object="rear_facade")
    check(
        "and moving the object throws that away",
        moved["cached"] is False,
        str(moved["cached"]),
    )

    print()
    if FAILED:
        print(f"{len(FAILED)} failed:")
        for name, detail in FAILED:
            print(f"  - {name}: {detail}")
        return 1
    print(f"passed: {PASSED}")
    print("scene surface queries verified")
    return 0


if __name__ == "__main__":
    code = main()
    if code:
        sys.exit(code)

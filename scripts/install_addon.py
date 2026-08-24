"""Install the bridge add-on into a local Blender.

Copies (or links) ``blender_extension/`` into Blender's user extensions
directory. A symlink is the default on platforms that allow one without extra
privileges, so editing the source is immediately live in Blender -- which is
what you want while developing, and harmless otherwise since the add-on is
plain Python.

    python scripts/install_addon.py                 # auto-detect Blender
    python scripts/install_addon.py --blender "C:/Program Files/Blender Foundation/Blender 4.2/blender.exe"
    python scripts/install_addon.py --copy          # copy instead of link
    python scripts/install_addon.py --uninstall

Nothing here touches the network, and nothing is executed except Blender itself,
which is asked only for its configuration directory.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import platform
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "blender_extension"
#: The directory name inside Blender. Must match the manifest id so Blender
#: treats a source install and a packaged install as the same extension.
TARGET_NAME = "blender_mcp_bridge"

#: Printed by Blender to tell us where its user scripts live.
PROBE = (
    "import bpy, json, sys;"
    "sys.stdout.write('BLENDER_MCP_PATHS' + json.dumps({"
    "'version': list(bpy.app.version),"
    "'user_scripts': bpy.utils.user_resource('SCRIPTS'),"
    "'user_extensions': bpy.utils.user_resource('EXTENSIONS'),"
    "}))"
)


def find_blender(explicit: str | None) -> pathlib.Path:
    if explicit:
        path = pathlib.Path(explicit)
        if not path.exists():
            raise SystemExit(f"No Blender at {path}")
        return path

    found = shutil.which("blender")
    if found:
        return pathlib.Path(found)

    candidates: list[pathlib.Path] = []
    system = platform.system()
    if system == "Windows":
        for base in (r"C:\Program Files\Blender Foundation",):
            candidates += sorted(pathlib.Path(base).glob("Blender */blender.exe"), reverse=True)
    elif system == "Darwin":
        candidates.append(pathlib.Path("/Applications/Blender.app/Contents/MacOS/Blender"))
    else:
        candidates += [
            pathlib.Path("/usr/bin/blender"),
            pathlib.Path("/usr/local/bin/blender"),
            pathlib.Path("/snap/bin/blender"),
        ]

    for candidate in candidates:
        if candidate.exists():
            return candidate

    raise SystemExit(
        "Could not find Blender. Pass --blender with the path to the executable."
    )


def blender_paths(blender: pathlib.Path) -> dict:
    """Ask Blender where its user directories are.

    Guessing these from the version number is wrong often enough to matter:
    portable installs, Steam, snap and flatpak all put them somewhere else.
    """
    result = subprocess.run(
        [str(blender), "--background", "--factory-startup", "--python-expr", PROBE],
        capture_output=True,
        text=True,
        timeout=180,
    )
    marker = "BLENDER_MCP_PATHS"
    if marker not in result.stdout:
        sys.stderr.write(result.stdout + result.stderr)
        raise SystemExit("Blender did not report its paths; see the output above.")
    payload = result.stdout.split(marker, 1)[1]
    # Blender may print more after our line; the JSON object ends at its brace.
    depth = 0
    for index, character in enumerate(payload):
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return json.loads(payload[: index + 1])
    raise SystemExit("Blender printed a truncated path report.")


def target_directory(paths: dict) -> pathlib.Path:
    """Where the add-on should live.

    Blender 4.2+ has an extensions directory; older builds use addons/ under
    the scripts directory. The manifest supports both, so the only difference
    is where the files go.
    """
    version = tuple(paths.get("version", (0, 0, 0)))
    extensions = paths.get("user_extensions")
    if version >= (4, 2, 0) and extensions:
        return pathlib.Path(extensions) / "user_default" / TARGET_NAME
    return pathlib.Path(paths["user_scripts"]) / "addons" / TARGET_NAME


def remove(target: pathlib.Path) -> None:
    if target.is_symlink() or target.is_file():
        target.unlink()
    elif target.is_dir():
        shutil.rmtree(target)


def install(target: pathlib.Path, copy: bool) -> str:
    target.parent.mkdir(parents=True, exist_ok=True)
    remove(target)

    if not copy:
        try:
            target.symlink_to(SOURCE, target_is_directory=True)
            return "linked"
        except OSError:
            # Windows without developer mode refuses symlinks. Copying works
            # everywhere; it just needs re-running after an edit.
            pass

    shutil.copytree(
        SOURCE,
        target,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    return "copied"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--blender", help="path to the Blender executable")
    parser.add_argument(
        "--copy",
        action="store_true",
        help="copy the files instead of linking them",
    )
    parser.add_argument(
        "--uninstall",
        action="store_true",
        help="remove a previous installation and exit",
    )
    args = parser.parse_args()

    if not SOURCE.is_dir():
        raise SystemExit(f"No add-on source at {SOURCE}")

    blender = find_blender(args.blender)
    paths = blender_paths(blender)
    target = target_directory(paths)
    version = ".".join(str(part) for part in paths.get("version", ()))

    if args.uninstall:
        remove(target)
        print(f"Removed {target}")
        return 0

    how = install(target, args.copy)
    print(f"Blender {version} at {blender}")
    print(f"{how.capitalize()} the add-on into {target}")
    print()
    print("Now, in Blender: Edit > Preferences > Add-ons, search for 'MCP',")
    print("enable it, then use the MCP panel in the 3D viewport sidebar (press N)")
    print("to connect to the server.")
    if how == "copied":
        print()
        print("Note: files were copied, so re-run this script after changing the source.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

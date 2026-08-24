"""Package the bridge add-on into a zip Blender can install.

    python scripts/package_addon.py                 # -> dist/blender_mcp_bridge-0.1.0.zip
    python scripts/package_addon.py --out build/

The archive contains one top-level directory, which is what Blender's installer
expects. Compiled bytecode and editor droppings are excluded, and the file list
is sorted with fixed timestamps so the same source always produces a
byte-identical archive -- a build that changes when nothing changed is a build
nobody can verify.
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import re
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "blender_extension"
MANIFEST = SOURCE / "blender_manifest.toml"
PACKAGE_NAME = "blender_mcp_bridge"

EXCLUDE_DIRECTORIES = {"__pycache__", ".git", ".mypy_cache", ".pytest_cache"}
EXCLUDE_SUFFIXES = {".pyc", ".pyo", ".orig", ".rej"}

#: A fixed timestamp, so the archive is reproducible. Zip cannot store anything
#: before 1980.
EPOCH = (1980, 1, 1, 0, 0, 0)


def manifest_version() -> str:
    text = MANIFEST.read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit(f"No version in {MANIFEST}")
    return match.group(1)


def included_files() -> list[pathlib.Path]:
    files = []
    for path in SOURCE.rglob("*"):
        if not path.is_file():
            continue
        if any(part in EXCLUDE_DIRECTORIES for part in path.relative_to(SOURCE).parts):
            continue
        if path.suffix in EXCLUDE_SUFFIXES:
            continue
        files.append(path)
    return sorted(files)


def check_no_secrets(files: list[pathlib.Path]) -> None:
    """Refuse to package anything that looks like a credential.

    The add-on has no business holding a token -- credentials live in the
    server's environment -- so anything matching here is a mistake worth
    stopping the build for.
    """
    patterns = [
        re.compile(r"BLENDER_MCP_SKETCHFAB_TOKEN\s*=\s*['\"][^'\"]+['\"]"),
        re.compile(r"(?i)\b(api[_-]?key|secret|password)\s*=\s*['\"][^'\"]{8,}['\"]"),
    ]
    offenders = []
    for path in files:
        if path.suffix not in {".py", ".toml", ".json", ".txt", ".md"}:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for pattern in patterns:
            if pattern.search(text):
                offenders.append(path.relative_to(ROOT).as_posix())
                break
    if offenders:
        raise SystemExit(
            "Refusing to package: these files look like they contain credentials:\n  "
            + "\n  ".join(offenders)
        )


def build(destination: pathlib.Path) -> pathlib.Path:
    version = manifest_version()
    files = included_files()
    if not files:
        raise SystemExit(f"No files found under {SOURCE}")
    check_no_secrets(files)

    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / f"{PACKAGE_NAME}-{version}.zip"

    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zip_file:
        for path in files:
            relative = path.relative_to(SOURCE)
            info = zipfile.ZipInfo(f"{PACKAGE_NAME}/{relative.as_posix()}", date_time=EPOCH)
            info.compress_type = zipfile.ZIP_DEFLATED
            # 0644, and the regular-file bit: without this the entries unpack
            # with whatever mode the extracting tool guesses.
            info.external_attr = (0o100644) << 16
            zip_file.writestr(info, path.read_bytes())

    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        default=str(ROOT / "dist"),
        help="directory to write the archive into (default: dist/)",
    )
    args = parser.parse_args()

    archive = build(pathlib.Path(args.out))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    size = archive.stat().st_size

    print(f"{archive}")
    print(f"  {size:,} bytes")
    print(f"  sha256 {digest}")
    print()
    print("Install in Blender: Edit > Preferences > Add-ons > Install from Disk")
    return 0


if __name__ == "__main__":
    sys.exit(main())

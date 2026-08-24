"""Check that the numbers in the documentation are the numbers in the build.

    python scripts/check_docs.py
    python scripts/check_docs.py --write   # update the generated tables in place

A count in a README is a claim, and an unchecked claim rots within a week. This
reads the real registry (through the `tool_inventory` example), the real bridge
handler table, and the real error taxonomy, and compares them against what the
documentation says.

Needs a built server binary or a Rust toolchain; everything else is standard
library. Blender is not required -- the tool registry is a compile-time table
and the bridge operation list is read by parsing the add-on, not by importing
it.
"""

from __future__ import annotations

import argparse
import ast
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ADDON = ROOT / "blender_extension"

#: Files whose counts are checked. A number in any of these that disagrees with
#: the build is a failure, not a warning.
CHECKED = [
    "README.md",
    "docs/TOOL_CATEGORIES.md",
    "docs/MCP_TOOLS.md",
    "docs/ARCHITECTURE.md",
    "CONTRIBUTING.md",
]

#: Markers around a generated block. Everything between them is rewritten by
#: `--write`; everything outside is prose nobody generates.
BEGIN = "<!-- generated:{name} -->"
END = "<!-- /generated:{name} -->"


class Report:
    def __init__(self) -> None:
        self.failures: list[str] = []

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        print(f"{'ok  ' if ok else 'FAIL'} {name}" + (f": {detail}" if not ok and detail else ""))
        if not ok:
            self.failures.append(name)


# --- the real numbers -------------------------------------------------------


def tool_inventory() -> tuple[dict[str, tuple[int, int]], int, int]:
    """(category -> (tools, schema bytes)), total tools, total bytes."""
    # No `--release`: the registry is a compile-time table, so the counts and
    # schema sizes are identical either way, and CI has already built the debug
    # profile for the tests. Asking for release here would make this check pay
    # for a whole second build.
    output = subprocess.run(
        ["cargo", "run", "-q", "-p", "blender-mcp-server", "--example", "tool_inventory"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if output.returncode != 0:
        raise SystemExit(
            "could not run the tool_inventory example; build the workspace first.\n"
            + output.stderr[-2000:]
        )
    categories: dict[str, tuple[int, int]] = {}
    total_tools = total_bytes = 0
    for line in output.stdout.splitlines():
        match = re.match(r"^(\S+)\s+(\d+)\s+([\d.]+)K$", line.strip())
        if not match:
            continue
        name, count, kilobytes = match.group(1), int(match.group(2)), float(match.group(3))
        payload = (count, int(round(kilobytes * 1024)))
        if name == "total":
            total_tools, total_bytes = payload
        else:
            categories[name] = payload
    if not categories or not total_tools:
        raise SystemExit(f"could not parse the tool inventory:\n{output.stdout}")
    return categories, total_tools, total_bytes


def bridge_operations() -> int:
    """How many operations the add-on registers, counted by parsing it."""
    total = 0
    for path in sorted(ADDON.rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if not isinstance(node, ast.FunctionDef):
                continue
            for decorator in node.decorator_list:
                if (
                    isinstance(decorator, ast.Call)
                    and isinstance(decorator.func, ast.Name)
                    and decorator.func.id in {"op", "read", "external"}
                ):
                    total += 1
    return total


def error_codes() -> int:
    source = (ROOT / "crates" / "blender-protocol" / "src" / "error.rs").read_text(
        encoding="utf-8"
    )
    body = re.search(r"pub enum ErrorCode\s*\{(.*?)\n\}", source, re.DOTALL)
    if not body:
        raise SystemExit("could not find `pub enum ErrorCode` in error.rs")
    return len(re.findall(r"^\s*([A-Z][A-Za-z0-9]*)\s*,", body.group(1), re.MULTILINE))


def declared_version() -> str:
    for line in (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        match = re.match(r'^version\s*=\s*"([^"]+)"', line.strip())
        if match:
            return match.group(1)
    raise SystemExit("no version in Cargo.toml")


# --- generated blocks -------------------------------------------------------


def category_table(categories: dict[str, tuple[int, int]], total_tools: int, total_bytes: int) -> str:
    lines = ["| Category | Tools | Input schema |", "|---|---:|---:|"]
    for name, (count, size) in sorted(categories.items()):
        lines.append(f"| `{name}` | {count} | {size / 1024:.1f} KB |")
    lines.append(f"| **Total** | **{total_tools}** | **{total_bytes / 1024:.1f} KB** |")
    return "\n".join(lines)


def render(name: str, body: str) -> str:
    return f"{BEGIN.format(name=name)}\n{body}\n{END.format(name=name)}"


def replace_block(text: str, name: str, body: str) -> tuple[str, bool]:
    pattern = re.compile(
        re.escape(BEGIN.format(name=name)) + r".*?" + re.escape(END.format(name=name)),
        re.DOTALL,
    )
    if not pattern.search(text):
        return text, False
    return pattern.sub(lambda _m: render(name, body), text), True


# --- version consistency ----------------------------------------------------


def version_sources() -> dict[str, str]:
    found = {}
    found["Cargo.toml"] = declared_version()
    manifest = (ADDON / "blender_manifest.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', manifest, re.MULTILINE)
    found["blender_manifest.toml"] = match.group(1) if match else "?"
    config = (ADDON / "config.py").read_text(encoding="utf-8")
    match = re.search(r'^ADDON_VERSION\s*=\s*"([^"]+)"', config, re.MULTILINE)
    found["blender_extension/config.py"] = match.group(1) if match else "?"
    return found


def license_sources() -> dict[str, str]:
    found = {}
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^license\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    found["Cargo.toml"] = match.group(1) if match else "?"
    manifest = (ADDON / "blender_manifest.toml").read_text(encoding="utf-8")
    match = re.search(r'^license\s*=\s*\["SPDX:([^"]+)"\]', manifest, re.MULTILINE)
    found["blender_manifest.toml"] = match.group(1) if match else "?"
    license_text = (ROOT / "LICENSE").read_text(encoding="utf-8")
    found["LICENSE"] = (
        "Apache-2.0" if "Apache License" in license_text and "Version 2.0" in license_text
        else "unrecognised"
    )
    return found


# --- main -------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write", action="store_true",
        help="Rewrite the generated blocks instead of only checking them",
    )
    parser.add_argument("--json", action="store_true", help="Print the real numbers as JSON")
    args = parser.parse_args()

    categories, total_tools, total_bytes = tool_inventory()
    operations = bridge_operations()
    codes = error_codes()

    facts = {
        "tools": total_tools,
        "categories": len(categories),
        "tool_schema_bytes": total_bytes,
        "core_tools": categories.get("core", (0, 0))[0],
        "core_schema_bytes": categories.get("core", (0, 0))[1],
        "bridge_operations": operations,
        "error_codes": codes,
        "version": declared_version(),
        "by_category": {k: {"tools": v[0], "schema_bytes": v[1]} for k, v in categories.items()},
    }
    if args.json:
        print(json.dumps(facts, indent=2))
        return 0

    report = Report()
    print(
        f"registry: {total_tools} tools in {len(categories)} categories "
        f"({total_bytes / 1024:.1f} KB of input schema), "
        f"{operations} bridge operations, {codes} error codes"
    )
    print()

    table = category_table(categories, total_tools, total_bytes)
    for relative in CHECKED:
        path = ROOT / relative
        if not path.exists():
            report.check(f"{relative} exists", False, "missing")
            continue
        text = path.read_text(encoding="utf-8")
        updated, had_block = replace_block(text, "tool-categories", table)

        if had_block and args.write and updated != text:
            path.write_text(updated, encoding="utf-8")
            text = updated
            print(f"     rewrote the generated table in {relative}")
        elif had_block:
            report.check(f"{relative} generated table is current", updated == text,
                         "run `python scripts/check_docs.py --write`")

        # Numbers written in prose. Each is a claim about the build, so each is
        # checked against it.
        for label, pattern, expected in [
            # Three digits or more. Every per-category count is smaller than
            # that and is a different, equally true claim -- "10 tools" about
            # `core` must not be read as a wrong total.
            ("tool count", r"(\d{3,})\s+(?:typed\s+)?(?:MCP\s+)?tools\b", total_tools),
            ("category count", r"(\d+)\s+categories\b", len(categories)),
            ("bridge operations", r"(\d+)\s+bridge operations\b", operations),
            ("error codes", r"(\d+)\s+(?:typed\s+)?error codes\b", codes),
        ]:
            wrong = sorted({
                int(found) for found in re.findall(pattern, text) if int(found) != expected
            })
            report.check(
                f"{relative}: {label}", not wrong,
                f"says {wrong}, the build says {expected}",
            )

    versions = version_sources()
    report.check(
        "one version everywhere",
        len(set(versions.values())) == 1,
        ", ".join(f"{k}={v}" for k, v in versions.items()),
    )

    licences = license_sources()
    report.check(
        "one licence everywhere",
        len(set(licences.values())) == 1,
        ", ".join(f"{k}={v}" for k, v in licences.items()),
    )

    print()
    if report.failures:
        print(f"{len(report.failures)} check(s) failed")
        return 1
    print("documentation matches the build")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

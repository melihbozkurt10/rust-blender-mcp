"""The error taxonomy must be identical on both sides of the bridge.

Rust owns the list; the add-on mirrors it. A code that exists in only one place
is a code a caller can receive but not recognise, which is exactly the "parse
the prose to find out what went wrong" failure the taxonomy exists to prevent.

Runs anywhere Python does -- no Blender, no ``bpy``, no dependencies. Both
sides are read with a parser rather than imported, because importing the add-on
needs Blender and importing Rust is not a thing.

    python tests/protocol/test_error_parity.py
"""

from __future__ import annotations

import ast
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
RUST_ERRORS = ROOT / "crates" / "blender-protocol" / "src" / "error.rs"
PYTHON_PROTOCOL = ROOT / "blender_extension" / "protocol.py"

#: `ErrorCode::InvalidArgument => "INVALID_ARGUMENT",` inside `as_str`.
RUST_ARM = re.compile(r'ErrorCode::(\w+)\s*=>\s*"([A-Z0-9_]+)"')
#: `InvalidArgument,` inside the enum body.
RUST_VARIANT = re.compile(r"^\s{4}(\w+),\s*$")


def rust_codes() -> dict[str, str]:
    """Variant name -> wire string, from the `as_str` implementation."""
    source = RUST_ERRORS.read_text(encoding="utf-8")
    codes = dict(RUST_ARM.findall(source))
    if not codes:
        raise AssertionError(f"no error codes found in {RUST_ERRORS}")
    return codes


def rust_variants() -> list[str]:
    """Every variant declared in the enum body, in declaration order."""
    source = RUST_ERRORS.read_text(encoding="utf-8")
    start = source.index("pub enum ErrorCode {")
    end = source.index("\n}", start)
    body = source[start:end]
    return [
        match.group(1)
        for line in body.splitlines()
        if (match := RUST_VARIANT.match(line))
    ]


def python_codes() -> dict[str, str]:
    """Attribute name -> value, parsed from the mirrored class."""
    tree = ast.parse(PYTHON_PROTOCOL.read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and node.name == "ErrorCode":
            codes = {}
            for statement in node.body:
                if isinstance(statement, ast.Assign) and isinstance(
                    statement.value, ast.Constant
                ):
                    for target in statement.targets:
                        if isinstance(target, ast.Name):
                            codes[target.id] = statement.value.value
            return codes
    raise AssertionError(f"no ErrorCode class found in {PYTHON_PROTOCOL}")


def check(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def test_every_rust_code_exists_in_python() -> None:
    rust = set(rust_codes().values())
    python = set(python_codes().values())
    missing = sorted(rust - python)
    check(
        not missing,
        f"the add-on is missing these error codes: {missing}",
    )


def test_python_invents_no_codes_of_its_own() -> None:
    rust = set(rust_codes().values())
    python = set(python_codes().values())
    extra = sorted(python - rust)
    check(
        not extra,
        f"the add-on has error codes Rust does not know about: {extra}",
    )


def test_every_variant_has_a_wire_string() -> None:
    """A variant missing from `as_str` would not compile, but a variant whose
    string was copied from its neighbour would -- and would be indistinguishable
    at runtime."""
    codes = rust_codes()
    variants = rust_variants()
    missing = [variant for variant in variants if variant not in codes]
    check(not missing, f"these variants have no wire string: {missing}")

    strings = list(codes.values())
    duplicates = sorted({s for s in strings if strings.count(s) > 1})
    check(not duplicates, f"two variants share a wire string: {duplicates}")


def test_python_names_match_their_values() -> None:
    """`INVALID_ARGUMENT = "INVALID_PATH"` would pass every other check here."""
    mismatched = [
        f"{name} = {value!r}"
        for name, value in python_codes().items()
        if name != value
    ]
    check(
        not mismatched,
        f"these attributes do not match their own values: {mismatched}",
    )


def test_screaming_snake_case_is_the_wire_format() -> None:
    for variant, wire in rust_codes().items():
        expected = re.sub(r"(?<!^)(?=[A-Z])", "_", variant).upper()
        check(
            wire == expected,
            f"`{variant}` serialises as `{wire}`, not `{expected}`; serde's "
            f"SCREAMING_SNAKE_CASE rename would disagree with `as_str`",
        )


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test()
        except AssertionError as error:
            failures += 1
            print(f"FAIL {test.__name__}: {error}")
        else:
            print(f"ok   {test.__name__}")

    total = len(rust_codes())
    print(f"\n{len(tests) - failures}/{len(tests)} checks passed over {total} error codes")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

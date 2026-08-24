"""Check the invariants this project would be worthless without.

Runs anywhere Python does: no Blender, no Rust toolchain, no dependencies.

    python scripts/verify_repo.py
    python scripts/verify_repo.py --quiet     # only failures

What it checks:

* The add-on contains no dynamic execution at all -- no ``eval``, ``exec``,
  ``compile``, ``__import__``, ``subprocess`` or ``os.system``. Checked by
  parsing the syntax tree, not by grepping, so a docstring that *mentions*
  ``eval`` does not trip it and a real call cannot hide behind formatting.
* Handlers are only ever registered through the decorators, so the dispatch
  table stays a finite, readable list.
* No credentials are committed.
* The workspace members all exist, and every crate is a member.
* The error taxonomy matches between Rust and Python.

This is the check to run before committing. It is fast and it is boring, which
is what a guard rail should be.
"""

from __future__ import annotations

import argparse
import ast
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ADDON = ROOT / "blender_extension"
CRATES = ROOT / "crates"

#: Callables that turn data into code. None of these belong in a process that
#: receives network input, however carefully they are used.
FORBIDDEN_CALLS = {
    "eval",
    "exec",
    "compile",
    "__import__",
    "execfile",
}
#: Modules that let one process start another.
FORBIDDEN_IMPORTS = {"subprocess", "runpy", "pty", "ctypes", "multiprocessing"}
#: Attribute calls that reach the operating system.
FORBIDDEN_ATTRIBUTES = {
    ("os", "system"),
    ("os", "popen"),
    ("os", "execv"),
    ("os", "execve"),
    ("os", "spawnv"),
    ("importlib", "import_module"),
}

SECRET_PATTERNS = [
    re.compile(r"BLENDER_MCP_SKETCHFAB_TOKEN\s*=\s*['\"][^'\"]{4,}['\"]"),
    re.compile(r"(?i)\b(api[_-]?key|access[_-]?token|password)\s*[=:]\s*['\"][^'\"]{8,}['\"]"),
    re.compile(r"\bsk-[A-Za-z0-9]{16,}"),
]


class Report:
    def __init__(self, quiet: bool) -> None:
        self.quiet = quiet
        self.failures: list[str] = []
        self.checks = 0

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.checks += 1
        if ok:
            if not self.quiet:
                print(f"ok   {name}")
        else:
            self.failures.append(f"{name}: {detail}")
            print(f"FAIL {name}: {detail}")


def python_files(directory: pathlib.Path) -> list[pathlib.Path]:
    return sorted(
        path
        for path in directory.rglob("*.py")
        if "__pycache__" not in path.parts
    )


def relative(path: pathlib.Path) -> str:
    return path.relative_to(ROOT).as_posix()


def dynamic_execution(path: pathlib.Path) -> list[str]:
    """Every dynamic-execution construct in one file, as `file:line reason`."""
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    found = []

    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            function = node.func
            if isinstance(function, ast.Name) and function.id in FORBIDDEN_CALLS:
                found.append(f"{relative(path)}:{node.lineno} calls {function.id}()")
            elif isinstance(function, ast.Attribute) and isinstance(function.value, ast.Name):
                pair = (function.value.id, function.attr)
                if pair in FORBIDDEN_ATTRIBUTES:
                    found.append(
                        f"{relative(path)}:{node.lineno} calls {pair[0]}.{pair[1]}()"
                    )
        elif isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".")[0]
                if root in FORBIDDEN_IMPORTS:
                    found.append(f"{relative(path)}:{node.lineno} imports {alias.name}")
        elif isinstance(node, ast.ImportFrom):
            root = (node.module or "").split(".")[0]
            if root in FORBIDDEN_IMPORTS:
                found.append(f"{relative(path)}:{node.lineno} imports from {node.module}")

    return found


def handler_registrations(path: pathlib.Path) -> list[str]:
    """Assignments into HANDLERS outside the dispatcher itself.

    The dispatcher owns the table; anything else writing to it would be a way
    to add an operation without the decorator, and therefore without the
    side-effect classification the whole retry and batching policy depends on.
    """
    if path.name == "dispatcher.py":
        return []
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if (
                    isinstance(target, ast.Subscript)
                    and isinstance(target.value, ast.Name)
                    and target.value.id in {"HANDLERS", "OP_KINDS"}
                ):
                    found.append(f"{relative(path)}:{node.lineno} writes to {target.value.id}")
    return found


def committed_secrets() -> list[str]:
    found = []
    for path in ROOT.rglob("*"):
        if not path.is_file():
            continue
        parts = path.relative_to(ROOT).parts
        if any(part in {".git", "target", "__pycache__", "dist", "node_modules"} for part in parts):
            continue
        if path.suffix not in {".py", ".rs", ".toml", ".json", ".md", ".txt", ".yml", ".yaml"}:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for pattern in SECRET_PATTERNS:
            match = pattern.search(text)
            if match:
                line = text[: match.start()].count("\n") + 1
                found.append(f"{relative(path)}:{line}")
                break
    return found


def workspace_members() -> tuple[list[str], list[str]]:
    """Members declared but missing, and crates present but not declared."""
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    block = re.search(r"members\s*=\s*\[(.*?)\]", manifest, re.S)
    declared = re.findall(r'"([^"]+)"', block.group(1)) if block else []

    missing = [name for name in declared if not (ROOT / name / "Cargo.toml").is_file()]
    present = {
        f"crates/{path.parent.name}"
        for path in CRATES.glob("*/Cargo.toml")
    }
    undeclared = sorted(present - set(declared))
    return missing, undeclared


def error_parity() -> list[str]:
    """Delegate to the parity test, so there is one definition of the rule."""
    sys.path.insert(0, str(ROOT / "tests" / "protocol"))
    try:
        import test_error_parity  # type: ignore[import-not-found]
    except ImportError as error:  # pragma: no cover - only if the file moves
        return [f"could not load the parity test: {error}"]

    problems = []
    for name in dir(test_error_parity):
        if not name.startswith("test_"):
            continue
        try:
            getattr(test_error_parity, name)()
        except AssertionError as error:
            problems.append(f"{name}: {error}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--quiet", action="store_true", help="print only failures")
    args = parser.parse_args()
    report = Report(args.quiet)

    files = python_files(ADDON)
    report.check(
        "the add-on has sources to check",
        bool(files),
        f"no Python files under {relative(ADDON)}",
    )

    execution = [problem for path in files for problem in dynamic_execution(path)]
    report.check(
        "the add-on contains no dynamic execution",
        not execution,
        "; ".join(execution),
    )

    registrations = [problem for path in files for problem in handler_registrations(path)]
    report.check(
        "handlers are registered only through the decorators",
        not registrations,
        "; ".join(registrations),
    )

    handlers = count_handlers(files)
    report.check(
        "the dispatch table is populated",
        handlers >= 100,
        f"only {handlers} operations found",
    )

    secrets = committed_secrets()
    report.check(
        "no credentials are committed",
        not secrets,
        "; ".join(secrets),
    )

    missing, undeclared = workspace_members()
    report.check(
        "every workspace member exists",
        not missing,
        f"declared but missing: {missing}",
    )
    report.check(
        "every crate is a workspace member",
        not undeclared,
        f"present but undeclared: {undeclared}",
    )

    parity = error_parity()
    report.check(
        "the error taxonomy matches across the bridge",
        not parity,
        "; ".join(parity),
    )

    print()
    if report.failures:
        print(f"{len(report.failures)} of {report.checks} checks failed")
        return 1
    print(f"all {report.checks} checks passed ({handlers} bridge operations)")
    return 0


def count_handlers(files: list[pathlib.Path]) -> int:
    names = set()
    for path in files:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if not isinstance(node, ast.FunctionDef):
                continue
            for decorator in node.decorator_list:
                if (
                    isinstance(decorator, ast.Call)
                    and isinstance(decorator.func, ast.Name)
                    and decorator.func.id in {"op", "read", "external"}
                    and decorator.args
                    and isinstance(decorator.args[0], ast.Constant)
                ):
                    names.add(decorator.args[0].value)
    return len(names)


if __name__ == "__main__":
    sys.exit(main())

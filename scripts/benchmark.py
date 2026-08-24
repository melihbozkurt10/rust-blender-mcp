"""Run the benchmark suite and write the results.

    python scripts/benchmark.py                     # everything it can run
    python scripts/benchmark.py --quick             # smaller samples, fewer sizes
    python scripts/benchmark.py --only mcp_roundtrip context_footprint
    python scripts/benchmark.py --no-blender        # server-only suites
    python scripts/benchmark.py --out benchmarks/results

Writes ``latest.json`` (structured) and ``latest.md`` (readable) into the output
directory. Both record the machine, the OS, the Blender version and the project
commit, because a benchmark result without those is an anecdote.

Requires a release build::

    cargo build --release
"""

from __future__ import annotations

import argparse
import datetime as _datetime
import json
import os
import pathlib
import sys
import traceback

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from benchmarks import harness, suites  # noqa: E402


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--blender", default=None, help="Path to the Blender executable")
    parser.add_argument("--binary", default=None, help="Path to the blender-mcp binary")
    parser.add_argument(
        "--out",
        default=str(REPO_ROOT / "benchmarks" / "results"),
        help="Directory for latest.json and latest.md",
    )
    parser.add_argument(
        "--only", nargs="+", default=None, metavar="SUITE",
        help=f"Run only these suites. Available: {', '.join(suites.SUITES)}",
    )
    parser.add_argument(
        "--no-blender", action="store_true",
        help="Skip everything that needs a running Blender",
    )
    parser.add_argument(
        "--quick", action="store_true",
        help="Fewer samples and fewer sizes. Good for a sanity check, not for publishing.",
    )
    parser.add_argument(
        "--scale", type=float, default=None,
        help="Multiply every sample count (0.1 = a tenth). Defaults to 1.0, or 0.1 with --quick.",
    )
    parser.add_argument("--label", default=None, help="Note recorded alongside the results")
    return parser.parse_args()


def render_markdown(document: dict) -> str:
    env = document["environment"]
    lines: list[str] = []
    add = lines.append

    add("# Benchmark results")
    add("")
    add(f"Generated {document['generated_utc']} · rust-blender-mcp "
        f"{env['project_version']} ({env['git_commit']})")
    if document.get("label"):
        add("")
        add(f"**{document['label']}**")
    add("")
    add("## Environment")
    add("")
    add("| | |")
    add("|---|---|")
    add(f"| CPU | {env['cpu']} |")
    add(f"| Cores / threads | {env['cpu_cores_physical']} / {env['cpu_threads']} |")
    add(f"| RAM | {env['ram_gb']} GB |")
    add(f"| OS | {env['os']} |")
    add(f"| Blender | {env['blender']} |")
    add(f"| Build profile | {env['build_profile']} |")
    add(f"| Python (harness) | {env['python']} |")
    add("")

    results = document["results"]

    def stats_table(title: str, stats: dict) -> None:
        add(f"**{title}**")
        add("")
        add("| samples | min | p50 | p95 | p99 | max | mean | req/s |")
        add("|---:|---:|---:|---:|---:|---:|---:|---:|")
        add(
            f"| {stats['count']} | {stats['min_ms']:.3f} ms | {stats['p50_ms']:.3f} ms | "
            f"{stats['p95_ms']:.3f} ms | {stats['p99_ms']:.3f} ms | {stats['max_ms']:.3f} ms | "
            f"{stats['mean_ms']:.3f} ms | {stats['per_second']:.0f} |"
        )
        add("")

    def section(key: str, title: str) -> dict | None:
        data = results.get(key)
        if data is None:
            return None
        add(f"## {title}")
        add("")
        if data.get("skipped"):
            add(f"_Skipped: {data['reason']}_")
            add("")
            return None
        if data.get("error"):
            add(f"_Failed: {data['error']}_")
            add("")
            return None
        return data

    data = section("mcp_roundtrip", "MCP round trip (no Blender)")
    if data:
        add(f"Path: `{data['path']}`")
        add("")
        stats_table(f"`{data['tool']}`, {data['stats']['count']} warm requests", data["stats"])
        add(f"Client-side JSON encode/decode inside the harness: "
            f"{data['client_json_overhead_ms']:.4f} ms per request, included in the figures above.")
        add("")

    data = section("bridge_floor", "IPC floor (no Rust server, no bpy work)")
    if data:
        add(f"Path: `{data['path']}`  \nExcludes: {data['excludes']}")
        add("")
        intervals = data["pump_interval_ms"]
        if "busy_ms" in intervals:
            add(
                f"Main-thread pump cadence: {intervals['busy_ms']:.0f} ms while a session is "
                f"active, {intervals['idle_ms']:.0f} ms after "
                f"{intervals['active_window_ms']:.0f} ms of quiet."
            )
        add("")
        stats_table(f"`{data['op']}`", data["stats"])

    data = section("startup", "Startup")
    if data:
        add(data["note"])
        add("")
        add("| stage | samples | p50 | p95 | max |")
        add("|---|---:|---:|---:|---:|")
        for key, label in [
            ("server_ready", "MCP server spawn → initialized"),
            ("blender_connect", "Blender launch → bridge connected"),
            ("ready_to_operate", "Spawn → ready to operate"),
            ("first_capabilities_call", "First `blender.capabilities`"),
        ]:
            stats = data.get(key)
            if isinstance(stats, dict) and "p50_ms" in stats:
                add(
                    f"| {label} | {stats['count']} | {stats['p50_ms'] / 1000:.3f} s | "
                    f"{stats['p95_ms'] / 1000:.3f} s | {stats['max_ms'] / 1000:.3f} s |"
                )
        add("")

    data = section("blender_ops", "Blender operations, full stack")
    if data:
        add(f"Path: `{data['path']}`")
        add("")
        add(data["note"])
        add("")
        add("| operation | samples | p50 | p95 | p99 | ops/s |")
        add("|---|---:|---:|---:|---:|---:|")
        for name, stats in data["operations"].items():
            add(
                f"| `{name}` | {stats['count']} | {stats['p50_ms']:.2f} ms | "
                f"{stats['p95_ms']:.2f} ms | {stats['p99_ms']:.2f} ms | {stats['per_second']:.0f} |"
            )
        add("")

    data = section("sequential", "Sequential operations")
    if data:
        add(f"`{data['operation']}` — {data['note']}")
        add("")
        add("| operations | total | ops/s | p50 | p95 |")
        add("|---:|---:|---:|---:|---:|")
        for count, run in data["runs"].items():
            stats = run["stats"]
            add(
                f"| {run['operations']} | {run['total_s']:.2f} s | {run['ops_per_second']:.0f} | "
                f"{stats['p50_ms']:.2f} ms | {stats['p95_ms']:.2f} ms |"
            )
        add("")

    data = section("batch", "Batch vs individual")
    if data:
        add(data["note"])
        add("")
        add("| operations | individual | batched | speedup | per-op individual | per-op batched |")
        add("|---:|---:|---:|---:|---:|---:|")
        for _size, run in data["runs"].items():
            note = f" ({run['batch_chunks']} chunks)" if run.get("batch_chunks", 1) > 1 else ""
            add(
                f"| {run['operations']} | {run['individual_total_s']:.2f} s | "
                f"{run['batch_total_s']:.2f} s{note} | {run['speedup']}× | "
                f"{run['individual_per_op_ms']:.2f} ms | {run['batch_per_op_ms']:.2f} ms |"
            )
        add("")

    data = section("context_footprint", "Tool schema / context footprint")
    if data:
        add(f"Measured: {data['measured']}.  \nToken source: `{data['tokenizer']}` — "
            f"{data['tokenizer_note']}")
        add("")
        add("| categories | tools | schema | tokens | `tools/list` p50 |")
        add("|---|---:|---:|---:|---:|")
        for label, entry in data["sets"].items():
            add(
                f"| {label} | {entry['tool_count']} | {entry['schema_kb']:.1f} KB | "
                f"{entry['tokens']:,} | {entry['tools_list_ms']['p50_ms']:.3f} ms |"
            )
        add("")
        activation = data["activation"]
        add(
            f"Enabling a category mid-session: {activation['enable_ms']:.2f} ms, "
            f"re-listing afterwards {activation['relist_ms']:.2f} ms. "
            f"Disabling removes the tools again "
            f"({activation['tools_after_enable']} → {activation['tools_after_disable']}): "
            f"{'no stale tools left behind' if activation['disable_leaves_no_stale_tools'] else 'STALE TOOLS REMAIN'}."
        )
        add("")

    data = section("memory", "Memory (MCP server process)")
    if data:
        add(f"Metric: {data['metric']} on `{data['platform']}`.")
        add("")
        add("| point | RSS |")
        add("|---|---:|")
        for key, value in data.items():
            if key.endswith("_rss_mb"):
                label = key[:-7].replace("_", " ")
                add(f"| {label} | {value:.1f} MB |")
        add("")

    data = section("distribution", "Distribution size")
    if data:
        add("| item | size |")
        add("|---|---:|")
        if "binary" in data:
            add(f"| MCP server binary (`{data['binary']['path']}`) | {data['binary']['human']} |")
        extension = data.get("extension", {})
        if "human" in extension:
            add(f"| Blender extension (`{extension['file']}`) | {extension['human']} |")
        add(f"| **Combined download** | **{data['combined_download']['human']}** |")
        add(f"| Source tree (no `.git`, no `target/`) | {data['source_tree']['human']} |")
        cache = data.get("build_cache")
        if cache:
            add(f"| Developer build cache `target/` | {cache['human']} |")
        add("")
        if cache:
            add(f"_{cache['note']}_")
            add("")

    return "\n".join(lines) + "\n"


def main() -> int:
    args = parse_args()
    binary = harness.find_binary(args.binary)
    blender = None if args.no_blender else harness.find_blender(args.blender)

    scale = args.scale if args.scale is not None else (0.1 if args.quick else 1.0)
    context = suites.Context(
        binary=binary, blender_exe=blender, scale=scale, quick=args.quick
    )

    selected = args.only or (
        suites.SERVER_ONLY if args.no_blender and not args.only else list(suites.SUITES)
    )
    unknown = [name for name in selected if name not in suites.SUITES]
    if unknown:
        raise SystemExit(f"unknown suite(s): {', '.join(unknown)}")

    print(f"binary:  {binary}")
    print(f"blender: {blender or 'not used'}")
    print(f"scale:   {scale}")
    print(f"suites:  {', '.join(selected)}")
    print()

    results: dict[str, object] = {}
    for name in selected:
        print(f"-> {name} ... ", end="", flush=True)
        started = harness.Timer()
        try:
            results[name] = suites.SUITES[name](context)
            outcome = "skipped" if results[name].get("skipped") else "ok"  # type: ignore[union-attr]
        except Exception as error:  # noqa: BLE001
            results[name] = {
                "error": f"{type(error).__name__}: {error}",
                "traceback": traceback.format_exc()[-2000:],
            }
            outcome = "FAILED"
        print(f"{outcome} ({harness.Timer() - started:.1f}s)")

    document = {
        "generated_utc": _datetime.datetime.now(_datetime.UTC).strftime("%Y-%m-%d %H:%M:%SZ"),
        "label": args.label,
        "scale": scale,
        "environment": harness.environment(blender),
        "results": results,
    }

    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "latest.json").write_text(
        json.dumps(document, indent=2, sort_keys=False) + "\n", encoding="utf-8"
    )
    (out_dir / "latest.md").write_text(render_markdown(document), encoding="utf-8")

    print()
    print(f"wrote {out_dir / 'latest.json'}")
    print(f"wrote {out_dir / 'latest.md'}")

    failures = [name for name, value in results.items()
                if isinstance(value, dict) and value.get("error")]
    if failures:
        print(f"\nsuites that failed: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

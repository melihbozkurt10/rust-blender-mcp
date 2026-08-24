"""Shared machinery for the benchmark suites.

Nothing in here is imported by the server or the add-on: benchmarks observe the
product from outside, through the same stdio MCP transport a client uses, so a
measurement cannot accidentally become a runtime dependency.

The pieces:

* :class:`Timer` -- monotonic, high resolution, and the only clock used.
* :class:`Stats` -- min/mean/p50/p95/p99/max plus throughput.
* :class:`McpClient` -- a minimal MCP client over the server's stdio transport.
* :class:`Blender` -- a headless Blender running the bridge.
* :func:`environment` -- what the numbers were measured on.
* :func:`estimate_tokens` -- a documented, deterministic token estimator.
"""

from __future__ import annotations

import json
import os
import platform
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable, Sequence

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Every duration in this package is seconds measured with `perf_counter`, which
# is monotonic and the highest resolution CPython offers. `time.time` would be
# wall clock and can step backwards.
Timer = time.perf_counter


# --- statistics -------------------------------------------------------------


@dataclass
class Stats:
    """Summary of a sample of durations, in milliseconds."""

    count: int
    min_ms: float
    mean_ms: float
    p50_ms: float
    p95_ms: float
    p99_ms: float
    max_ms: float
    stdev_ms: float
    total_s: float
    per_second: float

    @classmethod
    def of(cls, samples_s: Sequence[float], total_s: float | None = None) -> "Stats":
        if not samples_s:
            raise ValueError("no samples")
        ordered = sorted(samples_s)
        total = sum(samples_s) if total_s is None else total_s

        def percentile(fraction: float) -> float:
            # Nearest-rank. With 10,000 samples the difference from an
            # interpolating definition is below the measurement noise, and
            # nearest-rank always returns a value that was actually observed.
            index = min(len(ordered) - 1, max(0, round(fraction * len(ordered)) - 1))
            return ordered[index]

        return cls(
            count=len(ordered),
            min_ms=ordered[0] * 1e3,
            mean_ms=statistics.fmean(ordered) * 1e3,
            p50_ms=percentile(0.50) * 1e3,
            p95_ms=percentile(0.95) * 1e3,
            p99_ms=percentile(0.99) * 1e3,
            max_ms=ordered[-1] * 1e3,
            stdev_ms=(statistics.stdev(ordered) * 1e3 if len(ordered) > 1 else 0.0),
            total_s=total,
            per_second=(len(ordered) / total if total > 0 else float("nan")),
        )

    def as_dict(self) -> dict[str, Any]:
        return {
            "count": self.count,
            "min_ms": round(self.min_ms, 4),
            "mean_ms": round(self.mean_ms, 4),
            "p50_ms": round(self.p50_ms, 4),
            "p95_ms": round(self.p95_ms, 4),
            "p99_ms": round(self.p99_ms, 4),
            "max_ms": round(self.max_ms, 4),
            "stdev_ms": round(self.stdev_ms, 4),
            "total_s": round(self.total_s, 4),
            "per_second": round(self.per_second, 1),
        }


# --- token estimation -------------------------------------------------------

#: Pre-tokenisation split, close to what byte-pair encoders do before merging:
#: contractions, runs of letters, runs of digits, and single punctuation marks,
#: each optionally preceded by one space.
_PRETOKEN = re.compile(r"""'(?:[sdmt]|ll|ve|re)|\s?[A-Za-z]+|\s?\d+|\s?[^\sA-Za-z\d]+|\s+""")


def estimate_tokens(text: str) -> int:
    """Estimate the token cost of ``text`` for a BPE-style tokenizer.

    This is an *estimate* and is labelled as one everywhere it is reported. It
    is not a substitute for a model's real tokenizer; it exists so the numbers
    are reproducible on a machine with no network access and no vendor
    tokenizer installed.

    The rule, in full:

    1. Split with :data:`_PRETOKEN`, which mirrors the pre-tokenisation stage of
       a byte-pair encoder: word runs, digit runs and punctuation runs never
       merge across their boundaries.
    2. A letter run costs ``ceil(len / 4)`` tokens. Four characters is the
       long-run average for English and for the identifier-shaped words that
       dominate a JSON schema.
    3. A digit run costs ``ceil(len / 3)``: encoders split numbers more finely
       than words.
    4. A punctuation run costs one token per character. JSON structure -- ``{``,
       ``"``, ``:``, ``,`` -- rarely merges.
    5. A whitespace run costs one token per four characters, minimum one.

    If ``tiktoken`` is importable, :func:`count_tokens` uses it instead and the
    result is reported as exact rather than estimated.
    """
    total = 0
    for piece in _PRETOKEN.findall(text):
        stripped = piece.lstrip()
        if not stripped:
            total += max(1, -(-len(piece) // 4))
        elif stripped[0].isalpha():
            total += -(-len(stripped) // 4)
        elif stripped[0].isdigit():
            total += -(-len(stripped) // 3)
        else:
            total += len(stripped)
    return total


def count_tokens(text: str) -> tuple[int, str]:
    """Token count and how it was obtained (``"tiktoken:<encoding>"`` or ``"estimate"``)."""
    try:
        import tiktoken  # type: ignore

        encoding = tiktoken.get_encoding("cl100k_base")
        return len(encoding.encode(text)), "tiktoken:cl100k_base"
    except Exception:
        return estimate_tokens(text), "estimate"


# --- environment ------------------------------------------------------------


def _cpu_name() -> str:
    if sys.platform == "win32":
        name = os.environ.get("PROCESSOR_IDENTIFIER", "")
        try:
            out = subprocess.run(
                ["powershell", "-NoProfile", "-Command",
                 "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name"],
                capture_output=True, text=True, timeout=30,
            )
            if out.returncode == 0 and out.stdout.strip():
                return out.stdout.strip()
        except Exception:
            pass
        return name or platform.processor()
    if sys.platform == "darwin":
        try:
            out = subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"],
                capture_output=True, text=True, timeout=30,
            )
            if out.returncode == 0:
                return out.stdout.strip()
        except Exception:
            pass
    try:
        with open("/proc/cpuinfo", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except Exception:
        pass
    return platform.processor() or platform.machine()


def _total_ram_gb() -> float:
    try:
        import psutil

        return round(psutil.virtual_memory().total / (1024 ** 3), 1)
    except Exception:
        return float("nan")


def blender_version(executable: str) -> str:
    try:
        out = subprocess.run(
            [executable, "--version"], capture_output=True, text=True, timeout=120
        )
        first = out.stdout.strip().splitlines()[0] if out.stdout.strip() else ""
        return first.strip() or "unknown"
    except Exception:
        return "unknown"


def project_version() -> str:
    path = os.path.join(REPO_ROOT, "Cargo.toml")
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                match = re.match(r'^version\s*=\s*"([^"]+)"', line.strip())
                if match:
                    return match.group(1)
    except Exception:
        pass
    return "unknown"


def git_commit() -> str:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=REPO_ROOT, capture_output=True, text=True, timeout=30,
        )
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return "uncommitted"


def environment(blender_exe: str | None) -> dict[str, Any]:
    """Everything a reader needs to judge whether the numbers transfer."""
    try:
        import psutil

        physical = psutil.cpu_count(logical=False)
        logical = psutil.cpu_count(logical=True)
    except Exception:
        physical, logical = None, os.cpu_count()
    return {
        "cpu": _cpu_name(),
        "cpu_cores_physical": physical,
        "cpu_threads": logical,
        "ram_gb": _total_ram_gb(),
        "os": f"{platform.system()} {platform.release()} ({platform.version()})",
        "machine": platform.machine(),
        "python": platform.python_version(),
        "blender": blender_version(blender_exe) if blender_exe else "not measured",
        "project_version": project_version(),
        "git_commit": git_commit(),
        "build_profile": "release",
    }


# --- process discovery ------------------------------------------------------


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def find_blender(explicit: str | None = None) -> str | None:
    if explicit:
        return explicit
    env = os.environ.get("BLENDER_EXECUTABLE")
    if env:
        return env
    found = shutil.which("blender")
    if found:
        return found
    for candidate in [
        r"C:\Program Files\Blender Foundation\Blender 5.1\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 4.5\blender.exe",
        r"C:\Program Files\Blender Foundation\Blender 4.2\blender.exe",
        "/usr/bin/blender",
        "/usr/local/bin/blender",
        "/Applications/Blender.app/Contents/MacOS/Blender",
    ]:
        if os.path.exists(candidate):
            return candidate
    return None


def find_binary(explicit: str | None = None) -> str:
    if explicit:
        return explicit
    suffix = ".exe" if os.name == "nt" else ""
    # Release first: benchmarking a debug build measures the debug build.
    for profile in ("release", "debug"):
        candidate = os.path.join(REPO_ROOT, "target", profile, f"blender-mcp{suffix}")
        if os.path.exists(candidate):
            return candidate
    raise SystemExit(
        "blender-mcp binary not found. Run `cargo build --release` first."
    )


# --- MCP client -------------------------------------------------------------


class McpClient:
    """A minimal MCP client over the server's stdio transport.

    Deliberately synchronous: latency is what is being measured, so a request
    is written, a reply is read, and nothing overlaps.
    """

    def __init__(self, command: list[str], env: dict[str, str] | None = None) -> None:
        self.command = command
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env or dict(os.environ),
            text=True,
            bufsize=1,
        )
        self._next_id = 0
        self._stderr: list[str] = []
        self._drain = threading.Thread(target=self._drain_stderr, daemon=True)
        self._drain.start()

    @property
    def pid(self) -> int:
        return self.process.pid

    def _drain_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self._stderr.append(line.rstrip())

    def stderr_tail(self, count: int = 20) -> str:
        return "\n".join(self._stderr[-count:])

    def _send(self, message: dict) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method: str, params: Any = None, timeout: float = 120.0) -> dict:
        self._next_id += 1
        message: dict[str, Any] = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            message["params"] = params
        self._send(message)
        return self._read_reply(self._next_id, timeout)

    def notify(self, method: str, params: Any = None) -> None:
        message: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self._send(message)

    def _read_reply(self, request_id: int, timeout: float) -> dict:
        assert self.process.stdout is not None
        deadline = Timer() + timeout
        while Timer() < deadline:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError(f"server closed stdout.\nstderr:\n{self.stderr_tail()}")
            line = line.strip()
            if not line:
                continue
            message = json.loads(line)
            if message.get("id") == request_id:
                return message
        raise TimeoutError(f"no reply to request {request_id} within {timeout}s")

    def initialize(self, name: str = "blender-mcp-bench") -> dict:
        reply = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": name, "version": "0.1.0"},
            },
        )
        self.notify("notifications/initialized")
        return reply

    def list_tools(self) -> list[dict]:
        return self.request("tools/list", {})["result"]["tools"]

    def call_tool(self, name: str, arguments: dict | None = None, timeout: float = 120.0) -> dict:
        reply = self.request(
            "tools/call", {"name": name, "arguments": arguments or {}}, timeout=timeout
        )
        if "error" in reply:
            raise RuntimeError(f"{name} failed at the protocol level: {reply['error']}")
        return reply["result"]

    def structured(self, result: dict) -> Any:
        if "structuredContent" in result:
            return result["structuredContent"]
        content = result.get("content") or []
        if content and content[0].get("type") == "text":
            return json.loads(content[0]["text"])
        return None

    def call_structured(self, name: str, arguments: dict | None = None, timeout: float = 120.0) -> Any:
        return self.structured(self.call_tool(name, arguments, timeout=timeout))

    def close(self) -> None:
        try:
            if self.process.stdin:
                self.process.stdin.close()
            self.process.wait(timeout=5)
        except Exception:
            self.process.kill()
            try:
                self.process.wait(timeout=5)
            except Exception:
                pass


# --- Blender bridge ---------------------------------------------------------


class Blender:
    """A headless Blender running the bridge add-on against ``port``."""

    def __init__(self, executable: str, port: int, workspace: str, seconds: float = 3600.0) -> None:
        self.executable = executable
        self.port = port
        self.ready_file = os.path.join(workspace, f"ready-{port}")
        self.process = subprocess.Popen(
            [
                executable,
                "--background",
                "--factory-startup",
                "--python",
                os.path.join(REPO_ROOT, "scripts", "run_bridge.py"),
                "--",
                "--port", str(port),
                "--seconds", str(seconds),
                "--ready-file", self.ready_file,
            ],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        self._output: list[str] = []
        self._drain = threading.Thread(target=self._drain_output, daemon=True)
        self._drain.start()

    def _drain_output(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self._output.append(line.rstrip())

    def output_tail(self, count: int = 20) -> str:
        return "\n".join(self._output[-count:])

    def wait_ready(self, timeout: float = 180.0) -> float:
        """Block until the bridge has connected. Returns seconds waited."""
        start = Timer()
        deadline = start + timeout
        while Timer() < deadline:
            if os.path.exists(self.ready_file):
                return Timer() - start
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"Blender exited early ({self.process.returncode}).\n{self.output_tail(40)}"
                )
            time.sleep(0.01)
        raise TimeoutError(f"Blender did not connect within {timeout}s\n{self.output_tail(40)}")

    def close(self) -> None:
        try:
            self.process.terminate()
            self.process.wait(timeout=20)
        except Exception:
            self.process.kill()
            try:
                self.process.wait(timeout=10)
            except Exception:
                pass


# --- a running stack --------------------------------------------------------


@dataclass
class Stack:
    """A server, optionally a Blender, and the workspace they share."""

    client: McpClient
    blender: Blender | None
    workspace: str
    port: int
    startup: dict[str, float] = field(default_factory=dict)

    def close(self) -> None:
        if self.blender is not None:
            self.blender.close()
        self.client.close()
        shutil.rmtree(self.workspace, ignore_errors=True)


def start_stack(
    binary: str,
    blender_exe: str | None,
    *,
    categories: Iterable[str] | None = None,
    eager: bool = False,
    with_blender: bool = True,
    log: str = "error",
) -> Stack:
    """Start the server, connect Blender, and hand back a live stack."""
    port = free_port()
    workspace = tempfile.mkdtemp(prefix="blender-mcp-bench-")
    env = dict(os.environ)
    env.update(
        {
            "BLENDER_MCP_PORT": str(port),
            "BLENDER_MCP_WORKSPACE": workspace,
            "BLENDER_MCP_LOG": log,
        }
    )
    # An inherited value would silently override what this call asked for.
    env.pop("BLENDER_MCP_EAGER_TOOLS", None)
    env.pop("BLENDER_MCP_CATEGORIES", None)
    if eager:
        env["BLENDER_MCP_EAGER_TOOLS"] = "1"
    elif categories:
        env["BLENDER_MCP_CATEGORIES"] = ",".join(categories)

    timings: dict[str, float] = {}
    spawn_at = Timer()
    client = McpClient([binary], env)
    client.initialize()
    timings["server_ready_s"] = Timer() - spawn_at

    blender = None
    if with_blender:
        if blender_exe is None:
            raise SystemExit("Blender is required for this benchmark but was not found.")
        blender_at = Timer()
        blender = Blender(blender_exe, port, workspace)
        blender.wait_ready()
        timings["blender_connect_s"] = Timer() - blender_at
        timings["ready_to_operate_s"] = Timer() - spawn_at
        # `ready` is written the moment the socket handshake lands; the first
        # real operation is what proves the stack works end to end.
        client.call_structured("blender.status")

    return Stack(client=client, blender=blender, workspace=workspace, port=port, startup=timings)


# --- misc -------------------------------------------------------------------


def repeat(times: int, action: Callable[[int], None]) -> list[float]:
    """Run ``action`` ``times`` times, returning one duration per call."""
    samples: list[float] = []
    for index in range(times):
        start = Timer()
        action(index)
        samples.append(Timer() - start)
    return samples


def directory_size(path: str, exclude: Iterable[str] = ()) -> int:
    excluded = {os.path.normcase(e) for e in exclude}
    total = 0
    for root, dirs, files in os.walk(path):
        dirs[:] = [d for d in dirs if os.path.normcase(d) not in excluded]
        for name in files:
            try:
                total += os.path.getsize(os.path.join(root, name))
            except OSError:
                pass
    return total


def human_bytes(size: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if abs(size) < 1024 or unit == "GB":
            return f"{size:.1f} {unit}" if unit != "B" else f"{int(size)} B"
        size /= 1024
    return f"{size:.1f} GB"

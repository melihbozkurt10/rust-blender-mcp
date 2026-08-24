"""Run the bridge inside a headless Blender.

    blender --background --python scripts/run_bridge.py -- --port 9877 --seconds 60

In background mode Blender exits as soon as the script returns, and its timer
system has no event loop to drive it, so the main-thread pump is driven here
explicitly. That is the same function the timer calls when Blender has a UI --
the pump is not aware of which one is running it.
"""

from __future__ import annotations

import argparse
import os
import sys
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)

import bpy  # noqa: E402

from blender_extension import config, dispatcher, events, transport  # noqa: E402
from blender_extension import operations  # noqa: E402  (registers handlers)


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description="Run the Blender MCP bridge headless")
    parser.add_argument("--host", default=config.DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=config.DEFAULT_PORT)
    parser.add_argument(
        "--seconds",
        type=float,
        default=120.0,
        help="How long to stay connected before exiting",
    )
    parser.add_argument(
        "--ready-file",
        default=None,
        help="Write this file once connected, so a test harness can stop polling",
    )
    return parser.parse_args(argv)


def main() -> int:
    args = parse_args()
    print(
        f"[run_bridge] blender {bpy.app.version_string}, "
        f"{operations.operation_count()} operations",
        flush=True,
    )

    events.register()
    transport.BRIDGE.start(args.host, args.port, auto_reconnect=True)

    deadline = time.monotonic() + args.seconds
    announced = False
    try:
        while time.monotonic() < deadline:
            # The pump returns how long it wants before the next call, exactly
            # as it does for `bpy.app.timers`. Honouring it here means the
            # headless path has the same latency behaviour as the add-on, so a
            # measurement taken through this script describes the real thing.
            delay = dispatcher.pump()
            if not announced and dispatcher.STATE.connected:
                announced = True
                print(f"[run_bridge] connected to {args.host}:{args.port}", flush=True)
                if args.ready_file:
                    with open(args.ready_file, "w", encoding="utf-8") as handle:
                        handle.write(str(dispatcher.STATE.session_id or ""))
            time.sleep(delay)
    except KeyboardInterrupt:
        print("[run_bridge] interrupted", flush=True)
    finally:
        transport.BRIDGE.stop()
        events.unregister()

    stats = dispatcher.STATE.stats
    print(
        f"[run_bridge] done: {stats['requests']} requests, {stats['errors']} errors, "
        f"{stats['events']} events",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

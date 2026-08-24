"""The operation dispatcher.

This is the security boundary. Network input selects a handler from a fixed
table by exact name; it never becomes Python source, an attribute path, or an
operator name. There is no ``eval``, no ``exec``, no ``getattr`` on
caller-supplied strings without an allowlist, and no code path that can be
reached by inventing an ``op``.
"""

from __future__ import annotations

import math
import time
import traceback
from typing import Any, Callable

import bpy

from . import config, ids
from .protocol import BridgeError, ErrorCode, invalid_argument

#: op name -> handler. Populated by the ``operations`` package at import time.
HANDLERS: dict[str, Callable[["Context", dict], Any]] = {}

#: op name -> side-effect class, mirrored from the Rust classification.
OP_KINDS: dict[str, str] = {}


def op(name: str, kind: str = "WRITE"):
    """Register a handler under an exact operation name."""

    def decorate(function: Callable[["Context", dict], Any]):
        if name in HANDLERS:
            raise RuntimeError(f"duplicate handler registration for `{name}`")
        HANDLERS[name] = function
        OP_KINDS[name] = kind
        return function

    return decorate


def read(name: str):
    """Register a read-only handler."""
    return op(name, "READ")


def external(name: str):
    """Register a handler that writes outside the .blend file."""
    return op(name, "EXTERNAL_SIDE_EFFECT")


class Context:
    """What a handler gets besides its arguments."""

    def __init__(self, state: "BridgeState") -> None:
        self._state = state

    @property
    def session_id(self) -> str:
        return self._state.session_id or ""

    @property
    def revision(self) -> int:
        return self._state.revision

    def bump(self) -> int:
        """Advance the scene revision. Every mutation calls this."""
        return self._state.bump_revision()

    def emit(self, name: str, /, **fields: Any) -> None:
        """Queue an event for the server."""
        self._state.emit_event(name, **fields)

    @property
    def scene(self):
        return bpy.context.scene

    @property
    def view_layer(self):
        return bpy.context.view_layer


class BridgeState:
    """Mutable state shared by the pump, the transport and the event hooks.

    Held as a module-level singleton because Blender add-ons have no better
    place to put one, but every field it owns is either atomic or only touched
    from the main thread.
    """

    def __init__(self) -> None:
        self.session_id: str | None = None
        self.revision: int = 0
        self.outbox = None
        self.inbox = None
        self.connected: bool = False
        self.last_error: str | None = None
        self.server_address: str = f"{config.DEFAULT_HOST}:{config.DEFAULT_PORT}"
        self.stats = {"requests": 0, "errors": 0, "events": 0}
        # When the pump last handled something. Drives the pump cadence; see
        # `next_interval`. Starts far enough in the past to count as idle.
        self.last_activity: float = 0.0

    def bump_revision(self) -> int:
        self.revision += 1
        return self.revision

    def emit_event(self, name: str, /, **fields: Any) -> None:
        if self.outbox is None or self.session_id is None:
            return
        from . import protocol

        payload = protocol.event(self.session_id, self.revision, name, **fields)
        # Events are advisory. Dropping one when the queue is full is better
        # than blocking Blender's main thread on a slow reader.
        if self.outbox.put(payload, timeout=0):
            self.stats["events"] += 1

    def reset_for_new_session(self, session_id: str) -> None:
        self.session_id = session_id
        self.connected = True
        self.last_error = None
        # A server that has just connected always asks for status and
        # capabilities straight away, so start at the busy cadence rather than
        # making the first exchange of every session pay the idle interval.
        self.last_activity = time.monotonic()
        ids.invalidate_cache()


#: The one instance.
STATE = BridgeState()


def dispatch(request: dict[str, Any]) -> dict[str, Any]:
    """Run one request and build its response frame.

    Runs on Blender's main thread. Never raises: a handler that explodes
    produces an error response, because a raising pump would stop the timer and
    freeze the bridge.
    """
    from . import protocol

    request_id = request.get("request_id")
    command = request.get("command") or {}
    name = command.get("op")
    args = command.get("args") or {}

    if not isinstance(request_id, str):
        # Nothing to correlate a response with; the transport reports this as a
        # fatal frame instead.
        raise BridgeError(ErrorCode.INVALID_ARGUMENT, "request is missing `request_id`")

    STATE.stats["requests"] += 1

    if not isinstance(name, str) or not name:
        return protocol.error_response(
            request_id,
            invalid_argument("`command.op` must be a non-empty string."),
            STATE.revision,
        )
    if not isinstance(args, dict):
        return protocol.error_response(
            request_id,
            invalid_argument("`command.args` must be an object.", op=name),
            STATE.revision,
        )

    handler = HANDLERS.get(name)
    if handler is None:
        return protocol.error_response(
            request_id,
            BridgeError(
                ErrorCode.UNSUPPORTED_OPERATION,
                f"`{name}` is not an operation this add-on implements.",
                {"op": name, "closest": _closest_ops(name)},
            ),
            STATE.revision,
        )

    context = Context(STATE)
    try:
        result = handler(context, args)
    except BridgeError as error:
        STATE.stats["errors"] += 1
        return protocol.error_response(request_id, error, STATE.revision)
    except RuntimeError as error:
        # Blender raises RuntimeError for context and poll failures, which are
        # the single most common cause of a failed operation and deserve a
        # code the caller can act on.
        STATE.stats["errors"] += 1
        return protocol.error_response(
            request_id,
            BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR,
                str(error),
                {"op": name},
            ),
            STATE.revision,
        )
    except Exception as error:  # noqa: BLE001 - the pump must survive anything
        STATE.stats["errors"] += 1
        traceback.print_exc()
        return protocol.error_response(
            request_id,
            BridgeError(
                ErrorCode.BLENDER_INTERNAL_ERROR,
                f"{type(error).__name__}: {error}",
                {"op": name},
            ),
            STATE.revision,
        )

    return protocol.response(request_id, _sanitise(result), STATE.revision)


def _closest_ops(name: str, limit: int = 5) -> list[str]:
    """Operation names similar to one that was not found."""
    prefix = name.split(".")[0]
    same_namespace = sorted(o for o in HANDLERS if o.startswith(prefix))
    if same_namespace:
        return same_namespace[:limit]
    return sorted(HANDLERS)[:limit]


def _sanitise(value: Any) -> Any:
    """Make a handler's result JSON-encodable.

    ``NaN`` and infinities are the ones that matter: Blender produces them from
    degenerate geometry, and ``json.dumps`` would either emit invalid JSON or
    raise inside the writer thread where the error has nowhere to go.
    """
    if value is None:
        return {}
    if isinstance(value, float):
        if math.isnan(value) or math.isinf(value):
            return None
        return value
    if isinstance(value, (str, int, bool)):
        return value
    if isinstance(value, dict):
        return {str(k): _sanitise(v) for k, v in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [_sanitise(v) for v in value]
    # mathutils vectors, colours and Euler triples are all iterable.
    if hasattr(value, "__iter__"):
        try:
            return [_sanitise(v) for v in value]
        except TypeError:
            pass
    return str(value)


def pump() -> float:
    """Blender timer callback: run queued requests on the main thread.

    Returns the delay until the next call, which is what ``bpy.app.timers``
    expects -- and, since a request cannot be answered before the next call,
    that delay is most of every operation's latency. See :func:`next_interval`.
    """
    state = STATE
    if state.inbox is None or state.outbox is None:
        return config.PUMP_INTERVAL_IDLE

    from . import protocol

    handled = 0
    deadline = time.monotonic() + config.PUMP_BUDGET_SECONDS
    while time.monotonic() < deadline:
        request = state.inbox.get(timeout=None)
        if request is None:
            break
        handled += 1
        try:
            frame = dispatch(request)
        except BridgeError as error:
            frame = protocol.fatal(error)
        except Exception as error:  # noqa: BLE001
            traceback.print_exc()
            frame = protocol.fatal(
                BridgeError(ErrorCode.BLENDER_INTERNAL_ERROR, f"{type(error).__name__}: {error}")
            )
        # Blocking here would freeze the UI, so a full outbox drops the
        # response and the server's timeout reports it.
        if not state.outbox.put(frame, timeout=0):
            print("[blender-mcp] outbox full; dropped a response")

    if handled:
        state.last_activity = time.monotonic()
    return next_interval(state)


def next_interval(state: "BridgeState", now: float | None = None) -> float:
    """How long until the pump should run again.

    Three cases, in order:

    * Work is already queued. Come back immediately -- there is nothing to wait
      for and the budget above already bounded how long one pass may run.
    * Something was handled recently. Stay at the busy cadence, because a
      session that just made a request is very likely about to make another,
      and making the second one wait the idle interval is the whole cost this
      function exists to avoid.
    * Otherwise idle. A tick that finds an empty queue costs one non-blocking
      read, so the idle rate can stay high enough that the *first* request of a
      new burst is not noticeably delayed either.
    """
    now = time.monotonic() if now is None else now
    if state.inbox is not None and len(state.inbox):
        return config.PUMP_INTERVAL_BUSY
    if now - state.last_activity < config.PUMP_ACTIVE_WINDOW:
        return config.PUMP_INTERVAL_BUSY
    return config.PUMP_INTERVAL_IDLE


def registered_operations() -> list[str]:
    return sorted(HANDLERS)

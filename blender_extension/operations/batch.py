"""Run a run of already-validated operations in one main-thread pass.

Latency on this bridge is dominated by waiting for the next pump tick, not by
the operations themselves: measured on a release build, a round trip that does
no ``bpy`` work at all costs about the same as one that moves an object. A batch
sent one operation at a time therefore costs one tick per operation, which made
the server's ``batch.execute`` no faster than the same calls made individually
-- measurably slower, once its own bookkeeping was counted.

This handler runs a whole run inside a single tick.

It adds no capability. Every operation is looked up in the same fixed handler
table a single request uses, with the same argument decoding, the same error
mapping, and the same refusal to treat a name as anything but a key into that
table. What it removes is N-1 waits, not a check.
"""

from __future__ import annotations

import traceback
from typing import Any

from .. import dispatcher
from ..dispatcher import op
from ..protocol import BridgeError, ErrorCode, invalid_argument

#: Ceiling on one dispatch frame. The server enforces its own, lower limit; this
#: is the backstop for anything that reaches the bridge another way.
MAX_OPERATIONS = 1000

#: Operations that must not appear inside a run. ``batch.dispatch`` itself
#: because nesting it would let one frame expand without bound, and the
#: transaction verbs because the server owns the transaction lifecycle and a
#: run that opened or closed one behind its back would desynchronise it.
NOT_NESTABLE = frozenset(
    {
        "batch.dispatch",
        "transaction.begin",
        "transaction.commit",
        "transaction.rollback",
    }
)


def _run_one(ctx, name: str, args: dict) -> dict[str, Any]:
    """One operation, with the same error mapping the single-request path uses."""
    handler = dispatcher.HANDLERS.get(name)
    if handler is None:
        return {
            "ok": False,
            "error": BridgeError(
                ErrorCode.UNSUPPORTED_OPERATION,
                f"`{name}` is not an operation this add-on implements.",
                {"op": name},
            ).to_payload(),
        }
    try:
        return {"ok": True, "result": handler(ctx, args)}
    except BridgeError as error:
        return {"ok": False, "error": error.to_payload()}
    except RuntimeError as error:
        # Blender raises RuntimeError for context and poll failures.
        return {
            "ok": False,
            "error": BridgeError(
                ErrorCode.BLENDER_CONTEXT_ERROR, str(error), {"op": name}
            ).to_payload(),
        }
    except Exception as error:  # noqa: BLE001 - one bad step must not kill the pump
        traceback.print_exc()
        return {
            "ok": False,
            "error": BridgeError(
                ErrorCode.BLENDER_INTERNAL_ERROR,
                f"{type(error).__name__}: {error}",
                {"op": name},
            ).to_payload(),
        }


@op("batch.dispatch")
def dispatch_run(ctx, args: dict) -> dict[str, Any]:
    operations = args.get("operations")
    if not isinstance(operations, list) or not operations:
        raise invalid_argument("`operations` must be a non-empty array.")
    if len(operations) > MAX_OPERATIONS:
        raise BridgeError(
            ErrorCode.INVALID_ARGUMENT,
            f"A dispatch run may hold at most {MAX_OPERATIONS} operations; this one has "
            f"{len(operations)}.",
            {"limit": MAX_OPERATIONS, "given": len(operations)},
        )

    stop_on_error = bool(args.get("stop_on_error", True))
    undo_label = args.get("undo_label")
    if undo_label is not None and not isinstance(undo_label, str):
        raise invalid_argument("`undo_label`, when given, must be a string.")

    results: list[dict[str, Any]] = []
    stopped_at: int | None = None

    for index, entry in enumerate(operations):
        if not isinstance(entry, dict):
            raise invalid_argument(f"operations[{index}] must be an object.")
        name = entry.get("op")
        if not isinstance(name, str) or not name:
            raise invalid_argument(f"operations[{index}] has no `op`.")
        if name in NOT_NESTABLE:
            raise BridgeError(
                ErrorCode.INVALID_ARGUMENT,
                f"`{name}` cannot appear inside a dispatch run.",
                {"index": index, "op": name},
            )
        operation_args = entry.get("args") or {}
        if not isinstance(operation_args, dict):
            raise invalid_argument(f"operations[{index}].args must be an object.")

        outcome = _run_one(ctx, name, operation_args)
        outcome["index"] = index
        outcome["op"] = name
        results.append(outcome)
        dispatcher.STATE.stats["requests"] += 1
        if not outcome["ok"]:
            dispatcher.STATE.stats["errors"] += 1
            if stop_on_error:
                stopped_at = index
                break
        elif undo_label is not None:
            # An atomic batch needs one undo boundary per step, or rolling back
            # would undo the whole run as a single move.
            step = dispatcher.HANDLERS.get("transaction.step")
            if step is not None:
                try:
                    step(ctx, {"label": f"{index}: {name}"})
                except BridgeError:
                    # The server checked that a transaction was open before
                    # sending this; if it closed underneath us the commit will
                    # report it, and losing a boundary is not worth aborting
                    # work that already succeeded.
                    pass

    return {
        "results": results,
        "completed": sum(1 for entry in results if entry["ok"]),
        "total": len(operations),
        "stopped_at": stopped_at,
        "revision": ctx.revision,
    }

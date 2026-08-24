"""Undo-backed transactions.

Blender is not a database and this does not pretend otherwise. What it does
have is an undo stack, and for pure `.blend` mutations that is a real rollback
mechanism: pushing a boundary before a batch and stepping back to it afterwards
undoes exactly the operations in between.

Two honest limits, both reported rather than hidden:

* The undo stack needs a window. In background mode there is none, so a
  transaction is refused with `TRANSACTION_UNSUPPORTED` instead of silently
  doing nothing.
* Anything that touched the world outside the file -- a written render, an
  exported mesh, a downloaded asset -- is not undone by stepping back. The
  server refuses to put those operations in an atomic batch at all.
"""

from __future__ import annotations

import uuid
from typing import Any

import bpy

from ..dispatcher import op, read
from ..protocol import BridgeError, ErrorCode, invalid_argument
from . import _common as c

#: The transaction currently open, if any. One at a time: nesting undo
#: boundaries is not something Blender's stack models usefully.
_ACTIVE: dict[str, Any] | None = None


def _undo_available() -> tuple[bool, str]:
    if bpy.app.background:
        return False, (
            "Blender is running in background mode, where there is no undo stack. Atomic batches "
            "need a running Blender UI; use STOP_ON_ERROR mode instead."
        )
    if not hasattr(bpy.ops.ed, "undo_push"):
        return False, "This Blender build does not expose the undo stack to scripts."
    return True, ""


@read("transaction.supported")
def supported(ctx, args: dict) -> dict[str, Any]:
    available, reason = _undo_available()
    return {
        "supported": available,
        "reason": reason or None,
        "active": _ACTIVE["id"] if _ACTIVE else None,
        "revision": ctx.revision,
    }


@op("transaction.begin")
def begin(ctx, args: dict) -> dict[str, Any]:
    global _ACTIVE

    available, reason = _undo_available()
    if not available:
        raise BridgeError(ErrorCode.TRANSACTION_UNSUPPORTED, reason, {"background": bpy.app.background})

    if _ACTIVE is not None:
        raise BridgeError(
            ErrorCode.TRANSACTION_FAILED,
            "A transaction is already open. Commit or roll it back first.",
            {"active": _ACTIVE["id"]},
        )

    label = c.optional_str(args, "label", "MCP transaction") or "MCP transaction"
    bpy.ops.ed.undo_push(message=f"{label} (start)")

    _ACTIVE = {"id": str(uuid.uuid4()), "label": label, "steps": 0, "revision": ctx.revision}
    return {"transaction": _ACTIVE["id"], "label": label, "revision": ctx.revision}


@op("transaction.step")
def step(ctx, args: dict) -> dict[str, Any]:
    """Mark an undo boundary after one operation inside a transaction."""
    if _ACTIVE is None:
        raise BridgeError(
            ErrorCode.TRANSACTION_FAILED,
            "No transaction is open.",
            {},
        )
    label = c.optional_str(args, "label", "step") or "step"
    bpy.ops.ed.undo_push(message=f"{_ACTIVE['label']}: {label}")
    _ACTIVE["steps"] += 1
    return {"transaction": _ACTIVE["id"], "steps": _ACTIVE["steps"], "revision": ctx.revision}


@op("transaction.commit")
def commit(ctx, args: dict) -> dict[str, Any]:
    global _ACTIVE

    if _ACTIVE is None:
        raise BridgeError(ErrorCode.TRANSACTION_FAILED, "No transaction is open.", {})
    payload = {
        "transaction": _ACTIVE["id"],
        "steps": _ACTIVE["steps"],
        "committed": True,
        "revision": ctx.revision,
    }
    _ACTIVE = None
    return payload


@op("transaction.rollback")
def rollback(ctx, args: dict) -> dict[str, Any]:
    """Step the undo stack back to where the transaction began."""
    global _ACTIVE

    if _ACTIVE is None:
        raise BridgeError(ErrorCode.TRANSACTION_FAILED, "No transaction is open.", {})

    steps = _ACTIVE["steps"]
    undone = 0
    failure = None
    for _ in range(steps + 1):
        try:
            result = bpy.ops.ed.undo()
        except RuntimeError as error:
            failure = str(error)
            break
        if "CANCELLED" in result:
            failure = "Blender refused a further undo step; the stack may have been truncated."
            break
        undone += 1

    transaction_id = _ACTIVE["id"]
    _ACTIVE = None
    ctx.bump()

    if failure is not None:
        raise BridgeError(
            ErrorCode.ROLLBACK_FAILED,
            f"Rollback stopped after {undone} of {steps + 1} steps: {failure} The scene is in a "
            "partially reverted state; inspect it before continuing.",
            {"transaction": transaction_id, "undone": undone, "expected": steps + 1},
        )

    from .. import ids

    ids.invalidate_cache()
    return {
        "transaction": transaction_id,
        "rolled_back": True,
        "steps_undone": undone,
        "revision": ctx.revision,
    }

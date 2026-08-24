"""Thread-safe queues between the socket threads and Blender's main thread.

Nothing here touches ``bpy``. The reader thread only ever *enqueues*; the
main-thread pump is the only place Blender data is read or written.
"""

from __future__ import annotations

import queue as _queue
from typing import Any

from . import config


class Mailbox:
    """A bounded queue with a non-blocking drain."""

    def __init__(self, maxsize: int) -> None:
        self._queue: _queue.Queue = _queue.Queue(maxsize=maxsize)

    def put(self, item: Any, timeout: float | None = None) -> bool:
        """Enqueue, blocking up to ``timeout``. ``False`` when the queue is full.

        Blocking is the point: a full queue means Blender is behind, and
        pushing back through TCP is better than growing without limit.
        """
        try:
            self._queue.put(item, block=timeout is None or timeout > 0, timeout=timeout)
            return True
        except _queue.Full:
            return False

    def get(self, timeout: float | None = None) -> Any | None:
        try:
            return self._queue.get(block=timeout is not None, timeout=timeout)
        except _queue.Empty:
            return None

    def drain(self, limit: int | None = None) -> list[Any]:
        """Take up to ``limit`` items without blocking."""
        items: list[Any] = []
        while limit is None or len(items) < limit:
            try:
                items.append(self._queue.get_nowait())
            except _queue.Empty:
                break
        return items

    def clear(self) -> int:
        """Discard everything queued. Returns how many items were dropped."""
        return len(self.drain())

    def __len__(self) -> int:
        return self._queue.qsize()


def inbox() -> Mailbox:
    """Requests waiting for Blender's main thread."""
    return Mailbox(config.INBOX_MAX)


def outbox() -> Mailbox:
    """Responses and events waiting to go out on the socket."""
    return Mailbox(config.OUTBOX_MAX)

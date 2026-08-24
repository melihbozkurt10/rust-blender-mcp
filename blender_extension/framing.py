"""Length-prefixed JSON framing over a blocking socket.

The socket is used with a short timeout so the reader thread can notice a stop
request; ``recv`` returning nothing is the only reliable signal that the peer
closed, and everything else is a partial read to be continued.
"""

from __future__ import annotations

import json
import socket
import struct
from typing import Any

from . import config


class ConnectionClosed(Exception):
    """The peer closed the connection between frames."""


class FrameTooLarge(Exception):
    """A declared or produced frame exceeds the negotiated limit."""

    def __init__(self, size: int, limit: int) -> None:
        super().__init__(f"frame of {size} bytes exceeds the {limit} byte limit")
        self.size = size
        self.limit = limit


class MalformedFrame(Exception):
    """A frame arrived that was not valid UTF-8 JSON."""


def _recv_exactly(sock: socket.socket, count: int, should_stop) -> bytes:
    """Read exactly ``count`` bytes, tolerating short reads and timeouts.

    ``should_stop`` is polled between reads so a shutdown does not wait for the
    peer to send something.
    """
    chunks: list[bytes] = []
    remaining = count
    while remaining > 0:
        if should_stop():
            raise ConnectionClosed("stopping")
        try:
            chunk = sock.recv(min(remaining, 65536))
        except socket.timeout:
            # Expected: the timeout exists so this loop can re-check should_stop.
            continue
        except OSError as exc:
            raise ConnectionClosed(str(exc)) from exc
        if not chunk:
            raise ConnectionClosed("peer closed the connection")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(sock: socket.socket, should_stop=lambda: False) -> dict[str, Any]:
    """Read one frame and decode it."""
    header = _recv_exactly(sock, config.HEADER_LEN, should_stop)
    (size,) = struct.unpack(config.HEADER_FORMAT, header)
    if size > config.MAX_FRAME_BYTES:
        # The stream cannot be resynchronised after a bogus length, so this is
        # fatal for the connection.
        raise FrameTooLarge(size, config.MAX_FRAME_BYTES)
    if size == 0:
        raise MalformedFrame("empty frame")
    payload = _recv_exactly(sock, size, should_stop)
    try:
        return json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise MalformedFrame(str(exc)) from exc


def encode_frame(message: dict[str, Any]) -> bytes:
    """Encode one message, header included."""
    payload = json.dumps(message, separators=(",", ":"), allow_nan=False).encode("utf-8")
    size = len(payload)
    if size > config.MAX_FRAME_BYTES:
        raise FrameTooLarge(size, config.MAX_FRAME_BYTES)
    return struct.pack(config.HEADER_FORMAT, size) + payload


def write_frame(sock: socket.socket, message: dict[str, Any]) -> None:
    """Write one frame.

    Header and payload go out in a single ``sendall`` so a partial flush cannot
    leave the peer waiting on a body that never comes.
    """
    try:
        sock.sendall(encode_frame(message))
    except OSError as exc:
        raise ConnectionClosed(str(exc)) from exc

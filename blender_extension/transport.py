"""The socket side of the bridge.

Two threads: one reads frames and enqueues requests, one drains the outbox onto
the socket. Neither touches ``bpy`` -- Blender data is only ever read or written
by the main-thread pump in :mod:`dispatcher`. That separation is what keeps the
UI responsive and, more importantly, what keeps Blender from crashing: its data
API is not thread-safe and a stray write from a socket thread corrupts memory
rather than raising.
"""

from __future__ import annotations

import socket
import threading
import time
import uuid
from typing import Any

from . import compatibility, config, framing, protocol, queue as queues
from .dispatcher import STATE
from .protocol import BridgeError, ErrorCode


class Bridge:
    """Owns the connection lifecycle."""

    def __init__(self) -> None:
        self._thread: threading.Thread | None = None
        self._writer: threading.Thread | None = None
        self._socket: socket.socket | None = None
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self.host = config.DEFAULT_HOST
        self.port = config.DEFAULT_PORT
        self.auto_reconnect = True

    # -- lifecycle ----------------------------------------------------------

    def start(self, host: str, port: int, auto_reconnect: bool = True) -> None:
        """Begin connecting. Safe to call when already running."""
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            self.host = host
            self.port = port
            self.auto_reconnect = auto_reconnect
            self._stop.clear()
            STATE.inbox = queues.inbox()
            STATE.outbox = queues.outbox()
            STATE.server_address = f"{host}:{port}"
            self._thread = threading.Thread(
                target=self._run, name="blender-mcp-bridge", daemon=True
            )
            self._thread.start()

    def stop(self, timeout: float = 3.0) -> None:
        """Stop connecting and tear everything down.

        Must leave no live thread behind: repeatedly enabling and disabling the
        add-on during development is exactly how zombie threads accumulate.
        """
        self._stop.set()
        self._close_socket()
        thread = self._thread
        if thread is not None and thread.is_alive() and thread is not threading.current_thread():
            thread.join(timeout=timeout)
        writer = self._writer
        if writer is not None and writer.is_alive() and writer is not threading.current_thread():
            writer.join(timeout=timeout)
        with self._lock:
            self._thread = None
            self._writer = None
        STATE.connected = False
        STATE.session_id = None
        if STATE.inbox is not None:
            STATE.inbox.clear()
        if STATE.outbox is not None:
            STATE.outbox.clear()

    def is_running(self) -> bool:
        thread = self._thread
        return thread is not None and thread.is_alive()

    def reconnect(self) -> None:
        """Drop the current connection; the retry loop picks it up again."""
        self._close_socket()

    # -- internals ----------------------------------------------------------

    def _close_socket(self) -> None:
        with self._lock:
            sock, self._socket = self._socket, None
        if sock is not None:
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                # Already dead; nothing to do.
                pass
            try:
                sock.close()
            except OSError:
                pass

    def _run(self) -> None:
        delay = config.RECONNECT_INITIAL
        while not self._stop.is_set():
            try:
                connected = self._connect_once()
            except Exception as error:  # noqa: BLE001 - the retry loop must survive
                STATE.last_error = f"{type(error).__name__}: {error}"
                connected = False

            if self._stop.is_set():
                break
            if not self.auto_reconnect:
                break

            if connected:
                # A connection that lived and then ended gets a fresh, short
                # backoff: the server was there a moment ago.
                delay = config.RECONNECT_INITIAL
            self._sleep(delay)
            delay = min(delay * config.RECONNECT_MULTIPLIER, config.RECONNECT_MAX)

        STATE.connected = False
        STATE.session_id = None

    def _sleep(self, seconds: float) -> None:
        """Interruptible sleep."""
        self._stop.wait(timeout=seconds)

    def _connect_once(self) -> bool:
        """One connection attempt. ``True`` if a session was established."""
        try:
            sock = socket.create_connection((self.host, self.port), timeout=5.0)
        except OSError as error:
            STATE.last_error = f"cannot reach {self.host}:{self.port} ({error})"
            STATE.connected = False
            return False

        sock.settimeout(config.SOCKET_TIMEOUT)
        try:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass

        with self._lock:
            self._socket = sock

        try:
            session_id = self._handshake(sock)
        except (framing.ConnectionClosed, framing.MalformedFrame, framing.FrameTooLarge) as error:
            STATE.last_error = f"handshake failed: {error}"
            self._close_socket()
            return False
        except BridgeError as error:
            STATE.last_error = f"handshake refused: {error.message}"
            try:
                framing.write_frame(sock, protocol.fatal(error))
            except Exception:  # noqa: BLE001 - best effort
                pass
            self._close_socket()
            return False

        STATE.reset_for_new_session(session_id)
        STATE.last_error = None
        print(f"[blender-mcp] connected to {self.host}:{self.port} (session {session_id})")

        self._writer = threading.Thread(
            target=self._write_loop, args=(sock,), name="blender-mcp-writer", daemon=True
        )
        self._writer.start()

        try:
            self._read_loop(sock)
        finally:
            STATE.connected = False
            STATE.session_id = None
            self._close_socket()
            writer = self._writer
            if writer is not None and writer.is_alive():
                writer.join(timeout=1.0)
            self._writer = None
            print("[blender-mcp] disconnected")
        return True

    def _handshake(self, sock: socket.socket) -> str:
        frame = framing.read_frame(sock, self._stop.is_set)
        if frame.get("type") != "hello":
            raise BridgeError(
                ErrorCode.PROTOCOL_MISMATCH,
                f"expected a `hello` frame, got `{frame.get('type')}`",
                {"received": frame.get("type")},
            )

        their_version = frame.get("protocol_version")
        if their_version != config.PROTOCOL_VERSION:
            raise BridgeError(
                ErrorCode.PROTOCOL_MISMATCH,
                f"the server speaks protocol v{their_version}, this add-on speaks "
                f"v{config.PROTOCOL_VERSION}. Update whichever is older.",
                {
                    "server_protocol_version": their_version,
                    "addon_protocol_version": config.PROTOCOL_VERSION,
                },
            )

        # The server mints the session id; echoing it back is what proves this
        # connection belongs to the handshake the server just sent.
        session_id = frame.get("session_id") or str(uuid.uuid4())

        ack = protocol.hello_ack(
            session_id=session_id,
            identity=compatibility.identity(),
            capabilities=compatibility.capabilities(),
            revision=STATE.revision,
        )
        framing.write_frame(sock, ack)
        return session_id

    def _read_loop(self, sock: socket.socket) -> None:
        while not self._stop.is_set():
            try:
                frame = framing.read_frame(sock, self._stop.is_set)
            except framing.ConnectionClosed as error:
                STATE.last_error = str(error)
                return
            except framing.MalformedFrame as error:
                # One bad frame is one message's problem; the stream is still
                # synchronised because the length prefix was valid.
                print(f"[blender-mcp] ignoring an undecodable frame: {error}")
                continue
            except framing.FrameTooLarge as error:
                STATE.last_error = str(error)
                self._enqueue_out(
                    protocol.fatal(
                        BridgeError(ErrorCode.MESSAGE_TOO_LARGE, str(error)),
                    )
                )
                return

            kind = frame.get("type")
            if kind == "request":
                # Block briefly if Blender is behind: that backpressure is the
                # point of a bounded queue.
                if not STATE.inbox.put(frame, timeout=1.0):
                    request_id = frame.get("request_id")
                    if isinstance(request_id, str):
                        self._enqueue_out(
                            protocol.error_response(
                                request_id,
                                BridgeError(
                                    ErrorCode.RATE_LIMITED,
                                    "Blender is still working through queued operations.",
                                ),
                            )
                        )
            elif kind == "ping":
                self._enqueue_out(protocol.pong(int(frame.get("nonce") or 0)))
            elif kind == "pong":
                pass
            elif kind == "hello":
                print("[blender-mcp] ignoring a second hello on an established connection")
            else:
                print(f"[blender-mcp] ignoring an unexpected frame type: {kind}")

    def _enqueue_out(self, frame: dict[str, Any]) -> None:
        if STATE.outbox is not None:
            STATE.outbox.put(frame, timeout=0)

    def _write_loop(self, sock: socket.socket) -> None:
        while not self._stop.is_set():
            frame = STATE.outbox.get(timeout=0.1) if STATE.outbox is not None else None
            if frame is None:
                continue
            try:
                framing.write_frame(sock, frame)
            except framing.FrameTooLarge as error:
                # Replacing the payload keeps the request correlated instead of
                # letting the caller time out with no explanation.
                request_id = frame.get("request_id")
                if isinstance(request_id, str):
                    try:
                        framing.write_frame(
                            sock,
                            protocol.error_response(
                                request_id,
                                BridgeError(
                                    ErrorCode.MESSAGE_TOO_LARGE,
                                    f"The result was {error.size} bytes, over the "
                                    f"{error.limit} byte frame limit. Narrow the request "
                                    "with filters or pagination.",
                                    {"size_bytes": error.size, "limit_bytes": error.limit},
                                ),
                            ),
                        )
                    except Exception:  # noqa: BLE001
                        return
            except ValueError as error:
                # json.dumps refuses NaN and infinities. The dispatcher strips
                # them, so reaching here means something unusual got through.
                request_id = frame.get("request_id")
                if isinstance(request_id, str):
                    try:
                        framing.write_frame(
                            sock,
                            protocol.error_response(
                                request_id,
                                BridgeError(
                                    ErrorCode.BLENDER_INTERNAL_ERROR,
                                    f"The result could not be encoded as JSON: {error}",
                                ),
                            ),
                        )
                    except Exception:  # noqa: BLE001
                        return
            except framing.ConnectionClosed:
                return


#: The one bridge instance.
BRIDGE = Bridge()


def status() -> dict[str, Any]:
    """Snapshot for the UI panel and for diagnostics."""
    return {
        "running": BRIDGE.is_running(),
        "connected": STATE.connected,
        "address": STATE.server_address,
        "session_id": STATE.session_id,
        "protocol_version": config.PROTOCOL_VERSION,
        "addon_version": config.ADDON_VERSION,
        "revision": STATE.revision,
        "last_error": STATE.last_error,
        "queued_in": len(STATE.inbox) if STATE.inbox is not None else 0,
        "queued_out": len(STATE.outbox) if STATE.outbox is not None else 0,
        "stats": dict(STATE.stats),
    }


def wait_for_idle(timeout: float = 5.0) -> bool:
    """Block until both queues drain. Used by the headless smoke tests."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        inbox_empty = STATE.inbox is None or len(STATE.inbox) == 0
        outbox_empty = STATE.outbox is None or len(STATE.outbox) == 0
        if inbox_empty and outbox_empty:
            return True
        time.sleep(0.01)
    return False

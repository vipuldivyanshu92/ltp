"""
Python harness for the LTP C ABI (lab / Isaac-style extensions / LeRobot teleop).

Build the shared library first: `cargo build -p teleop-transport --release`

PYTHONPATH must include this directory when using `lerobot_teleoperate_ltp_leader_cam.py`.
Optional: set `LTP_LIBRARY_PATH` to the full path of `libteleop_transport.{so,dylib,dll}`.
"""
from __future__ import annotations

import ctypes
import json
import os
import sys
import time
from ctypes import POINTER, byref, c_char_p, c_int, c_size_t, c_uint16, c_uint32, c_uint64, Structure

# Mirror include/ltp.h
LTP_SCHEMA_LEROBOT_TELEOP_JSON = 0xE001
LTP_SCHEMA_LEROBOT_CAMERA_JSON = 0xE002
LTP_RECV_KIND_CONTROL = 0
LTP_RECV_KIND_VIDEO_JPEG = 1


class LtpConfig(Structure):
    _fields_ = [
        ("bind_port", c_uint16),
        ("peer_port", c_uint16),
        ("bind_host", c_char_p),
        ("peer_host", c_char_p),
    ]


def _lib_path() -> str:
    env = os.environ.get("LTP_LIBRARY_PATH")
    if env:
        return env
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    name = "libteleop_transport.dylib"
    if sys.platform.startswith("linux"):
        name = "libteleop_transport.so"
    elif sys.platform.startswith("win"):
        name = "teleop_transport.dll"
    return os.path.join(root, "target", "release", name)


def load():
    path = _lib_path()
    lib = ctypes.CDLL(path)
    lib.ltp_abi_version.restype = c_uint32
    lib.ltp_last_error.restype = c_uint32
    lib.ltp_session_create.argtypes = [POINTER(LtpConfig)]
    lib.ltp_session_create.restype = ctypes.c_void_p
    lib.ltp_session_destroy.argtypes = [ctypes.c_void_p]
    lib.ltp_send_control.argtypes = [
        ctypes.c_void_p,
        c_uint16,
        c_uint16,
        ctypes.c_void_p,
        c_size_t,
        c_uint32,
        c_uint64,
    ]
    lib.ltp_send_control.restype = c_int
    lib.ltp_poll_recv.argtypes = [ctypes.c_void_p, c_uint64]
    lib.ltp_poll_recv.restype = c_int
    lib.ltp_recv_pop.argtypes = [
        ctypes.c_void_p,
        POINTER(c_int),
        POINTER(c_uint16),
        ctypes.c_void_p,
        c_size_t,
        POINTER(c_size_t),
    ]
    lib.ltp_recv_pop.restype = c_int
    lib.ltp_send_twist_stub.argtypes = [ctypes.c_void_p, c_uint16, c_uint32]
    lib.ltp_send_twist_stub.restype = c_int
    return lib


class LtpSession:
    """Single-threaded session wrapper (do not call concurrently from multiple threads)."""

    def __init__(
        self,
        bind_host: str,
        bind_port: int,
        peer_host: str,
        peer_port: int,
        *,
        lib=None,
    ):
        self._lib = lib or load()
        abi = self._lib.ltp_abi_version()
        if abi != 2:
            raise RuntimeError(f"LTP ABI mismatch: expected 2 (ltp.h), library reports {abi}")
        cfg = LtpConfig(
            bind_port=int(bind_port) & 0xFFFF,
            peer_port=int(peer_port) & 0xFFFF,
            bind_host=bind_host.encode("ascii"),
            peer_host=peer_host.encode("ascii"),
        )
        self._ptr = self._lib.ltp_session_create(ctypes.byref(cfg))
        if not self._ptr:
            raise RuntimeError(f"ltp_session_create failed, last_error={self._lib.ltp_last_error()}")
        self._seq_by_stream: dict[int, int] = {}

    def close(self) -> None:
        if self._ptr:
            self._lib.ltp_session_destroy(self._ptr)
            self._ptr = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _next_seq(self, stream_id: int) -> int:
        s = self._seq_by_stream.get(stream_id, 0)
        self._seq_by_stream[stream_id] = (s + 1) & 0xFFFFFFFF
        return s

    def send_control(
        self,
        stream_id: int,
        schema_id: int,
        payload: bytes,
        *,
        timestamp_ns: int | None = None,
    ) -> None:
        if not self._ptr:
            raise RuntimeError("session closed")
        ts = time.time_ns() if timestamp_ns is None else timestamp_ns
        seq = self._next_seq(stream_id)
        n = len(payload)
        if n == 0:
            data_ptr = None
        else:
            buf = (ctypes.c_ubyte * n)(*payload)
            data_ptr = ctypes.cast(buf, ctypes.c_void_p)
        rc = self._lib.ltp_send_control(
            self._ptr,
            stream_id & 0xFFFF,
            schema_id & 0xFFFF,
            data_ptr,
            n,
            seq,
            ts,
        )
        if rc != 0:
            raise RuntimeError(f"ltp_send_control failed rc={rc} last_error={self._lib.ltp_last_error()}")

    def send_lerobot_action_json(self, stream_id: int, action, *, timestamp: float | None = None) -> None:
        ts = time.perf_counter() if timestamp is None else timestamp
        msg = json.dumps({"action": action, "timestamp": ts}).encode("utf-8")
        self.send_control(stream_id, LTP_SCHEMA_LEROBOT_TELEOP_JSON, msg, timestamp_ns=time.time_ns())

    def poll_recv(self, now_ns: int) -> None:
        if not self._ptr:
            raise RuntimeError("session closed")
        rc = self._lib.ltp_poll_recv(self._ptr, now_ns)
        if rc != 0:
            raise RuntimeError(f"ltp_poll_recv failed rc={rc} last_error={self._lib.ltp_last_error()}")

    def recv_pop_bytes(self) -> tuple[int, int, bytes] | None:
        """Returns (kind, schema_id, payload) or None if empty. schema_id is meaningful for CONTROL kind."""
        if not self._ptr:
            raise RuntimeError("session closed")
        cap = 256 * 1024
        while True:
            buf = ctypes.create_string_buffer(cap)
            out_len = c_size_t(0)
            kind = c_int(0)
            schema = c_uint16(0)
            rc = self._lib.ltp_recv_pop(
                self._ptr,
                byref(kind),
                byref(schema),
                buf,
                cap,
                byref(out_len),
            )
            if rc == 0:
                return None
            if rc == -2:
                need = int(out_len.value)
                cap = max(need + 4096, cap * 2)
                continue
            if rc != 1:
                raise RuntimeError(f"ltp_recv_pop failed rc={rc} last_error={self._lib.ltp_last_error()}")
            n = int(out_len.value)
            return (int(kind.value), int(schema.value), buf.raw[:n])


def smoke():
    lib = load()
    abi = lib.ltp_abi_version()
    if abi != 2:
        raise SystemExit(f"ABI mismatch: expected 2 got {abi}")
    cfg = LtpConfig(
        bind_port=0,
        peer_port=9,
        bind_host=b"127.0.0.1",
        peer_host=b"127.0.0.1",
    )
    p = lib.ltp_session_create(ctypes.byref(cfg))
    if not p:
        raise SystemExit(f"create failed, last_error={lib.ltp_last_error()}")
    try:
        rc = lib.ltp_send_twist_stub(p, 1, 1)
        if rc != 0:
            raise SystemExit(f"send failed rc={rc}")
    finally:
        lib.ltp_session_destroy(p)
    print("ltp_ctypes smoke OK")


if __name__ == "__main__":
    smoke()

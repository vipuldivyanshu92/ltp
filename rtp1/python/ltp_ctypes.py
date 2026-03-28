"""
Minimal Python harness for the LTP C ABI (lab / Isaac-style extensions).
Build the shared library first: `cargo build -p teleop-transport --release`
"""
from __future__ import annotations

import ctypes
import os
import sys
from ctypes import POINTER, c_char_p, c_int, c_size_t, c_uint16, c_uint32, c_uint64, Structure


class LtpConfig(Structure):
    _fields_ = [
        ("bind_port", c_uint16),
        ("peer_port", c_uint16),
        ("bind_host", c_char_p),
        ("peer_host", c_char_p),
    ]


def _lib_path() -> str:
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
    lib.ltp_send_twist_stub.argtypes = [ctypes.c_void_p, c_uint16, c_uint32]
    lib.ltp_send_twist_stub.restype = c_int
    return lib


def smoke():
    lib = load()
    abi = lib.ltp_abi_version()
    if abi != 1:
        raise SystemExit(f"ABI mismatch: expected 1 got {abi}")
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

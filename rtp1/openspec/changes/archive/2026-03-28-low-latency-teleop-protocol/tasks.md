## 1. Project scaffold and types

- [x] 1.1 Create Rust workspace layout (`teleop-transport` crate, optional `teleop-bridge` for ROS2) with release profile tuned for size (LTO, strip) and document target triples (x86_64, aarch64/Jetson).
- [x] 1.2 Define packed `LtpHeader` and enums (`PriorityClass`, `PayloadType`, flags) matching `design.md`; add `#[repr(C)]` tests for size and byte order.
- [x] 1.3 Add unit tests for parse/serialize round-trip and reject invalid `magic`, `version`, and `payload_len` bounds.

## 2. Socket manager and multi-path I/O

- [x] 2.1 Implement `PathHandle` (bound UDP socket, local iface, `path_tag`, send/recv buffers) and registration API for multiple paths.
- [x] 2.2 Implement batched receive loop (epoll/kqueue or polling abstraction) demuxing to a single ingress queue with `(path_id, raw_datagram)`.
- [x] 2.3 Implement duplicate-send and split-send policies per payload class; wire `path_tag` and metrics (RTT EWMA, loss estimate) per path.

## 3. Sender: framer and strict priority scheduler

- [x] 3.1 Implement per-`stream_id` allocators and framing helper that builds datagrams with correct header fields and extensions.
- [x] 3.2 Implement strict-priority multi-queue scheduler (Control/Haptic before Video/Telemetry) with optional telemetry starvation guard; integrate slice EDF within Video queue.
- [x] 3.3 Implement MTU-aware slicing for video path; ensure no IP fragmentation on hot path for default configs.

## 4. Receiver: dedup, reorder, video reassembly

- [x] 4.1 Implement deduplication for duplicate mode using `(stream_id, seq)` + path visibility rules from specs.
- [x] 4.2 Implement per control stream reorder buffer with `max_reorder` and ordered delivery callback.
- [x] 4.3 Implement video `(frame_id, slice_id)` reassembly map and deadline-based discard without blocking control processing.

## 5. FEC

- [x] 5.1 Implement configurable XOR or (k,n) RS parity groups for control micro-batches; decode path without NACK on hot control path.
- [x] 5.2 Implement video slice FEC groups with header/extension metadata; bound decode CPU and optional degrade path per `fec/spec.md`.

## 6. Integration and footprint

- [x] 6.1 Document and implement shared-memory ring API (or iceoryx-style segments) for control + compressed frames; keep DDS off hot path per `ros2-integration/spec.md`.
- [x] 6.2 Document encoder buffer ownership contract (NVENC/V4L2 export) and optional `MSG_ZEROCOPY` send path with fallbacks.
- [x] 6.3 Measure and document release binary size; adjust features/flags to meet 30 MB budget for core + bridge as scoped.

## 7. SDK and ecosystem integration

- [x] 7.1 Define and implement stable **C ABI** (`include/ltp.h`): session create/destroy, config, non-blocking send/recv, error codes, ABI version symbol; use `cbindgen` or hand-maintained parity with Rust core.
- [x] 7.2 Add **control payload envelope** with `control_schema_id` and reference schema registry (documented IDs for twist, joint deltas, gamepad, etc.).
- [x] 7.3 Ship **minimal Python bindings** (pyo3 or ctypes + stub) exercising the C API for lab and Isaac-style workflows.
- [x] 7.4 Write **integration guide**: ROS 2 bridge wiring + executor threading, C++ static/dynamic link, Python extension load path, Unity/Unreal native plugin notes (P/Invoke / `DllImport` / module load).
- [x] 7.5 Ensure ROS 2 bridge uses only the stable API per `ros2-integration` / `sdk-ecosystem-integration` (no duplicate framer).

## 8. Verification

- [x] 8.1 Add integration tests: multi-path duplicate and split scenarios with injected loss/reorder simulators.
- [x] 8.2 Add tests proving control precedence over video under contention and ordered control delivery under reorder.
- [x] 8.3 Add a smoke test or CI job that loads the shared library from a tiny C or Python harness to verify ABI/version checks per `sdk-ecosystem-integration`.

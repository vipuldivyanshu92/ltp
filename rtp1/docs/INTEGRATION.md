# LTP1 integration guide

This transport is meant to embed behind **one stable C ABI** (`include/ltp.h`). Do not reimplement header packing in ROS, Unity, Unreal, or vendor SDKs.

## ROS 2

- Build or vendor `libteleop_transport` (cdylib) beside your workspace.
- From `rclcpp` / `rclpy`, load the `.so` / `.dylib` and call `ltp_session_create`, `ltp_send_control`, `ltp_poll_recv`, `ltp_session_destroy`.
- Keep the **executor** off the hot path: run `ltp_poll_recv` from a dedicated high-priority thread; hand results to a lock-free queue consumed by your control node. Parameter discovery may use ROS params; **do not** serialize teleop samples through DDS on the hot loop (see `specs/ros2-integration/spec.md`).
- The `teleop-bridge` crate is a policy placeholder: production bridges should still call the C API so Python and C++ nodes share one implementation.

## C++ (static or dynamic)

- Add `include/` to include path; link `teleop_transport` (cdylib or static if you add a staticlib crate-type).
- Initialize `ltp_config` with ASCII host strings (`const char*`), ports in host byte order.
- On Windows, delay-load or explicit `LoadLibrary` + `GetProcAddress` is fine; symbol names are unmangled C.

## Python / Isaac Sim–style extensions

- Use `python/ltp_ctypes.py` as a starting point: it loads `target/release/libteleop_transport.*` and runs a smoke send.
- In Omniverse/Isaac, place the shared library where the extension can `ctypes.CDLL` it; prefer a pinned ABI (`ltp_abi_version()`).

## Unity / Unreal

- Unity: `[DllImport("teleop_transport")]` (name per platform) for `ltp_abi_version`, `ltp_session_create`, etc.; copy the library next to the player binary.
- Unreal: `FPlatformProcess::GetDllHandle` on the module path; resolve function pointers with `FPlatformProcess::GetDllExport`.

## Control schema mapping

- Application payloads use `ControlEnvelope` (`control_schema_id` + length + bytes). Reference IDs: `SCHEMA_TWIST`, `SCHEMA_JOINT_DELTA`, `SCHEMA_GAMEPAD` in `control_envelope.rs`.
- Map MoveIt / jog / gamepad nodes to these IDs without changing the LTP header.

## Threading

- `ltp_send_control` currently flushes the internal scheduler immediately (demo-friendly). For production, split enqueue vs flush if you add batching APIs later.
- Assume **one producer thread** per session unless you add external locking.

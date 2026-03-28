## Context

The platform needs a proprietary realtime transport for stereoscopic RGB video, high-rate IMU/sensor telemetry, and bidirectional control and force-feedback over long-RTT internet. Standard stacks optimize for generality and congestion-friendly TCP semantics; this project optimizes for **bounded glass-to-glass latency** (target &lt;50 ms end-to-end where physics and path allow), **non-blocking control**, **multi-path utilization**, and **FEC-first reliability** on the hot path. The reference implementation favors **Rust** (memory safety, async ecosystem) with a **&lt;30 MB** deployable binary and **ROS2** integration via shared memory or a thin bridge that keeps DDS off the realtime loop.

## Goals / Non-Goals

**Goals:**

- Define a **fixed-layout UDP (or QUIC unreliable datagram) header** with distinct sequence spaces or subspaces per logical stream, explicit **priority class**, **payload type**, monotonic **timestamps** (capture vs network time), and optional **FEC metadata**.
- Specify **multi-path bonding** with configurable **duplicate** vs **split** modes, per-path scheduling, and receiver-side **dedup + ordering** without stalling the control path.
- Specify **sender-side strict priority**: control and haptic traffic preempt video slice and bulk telemetry scheduling; video loss does not delay control.
- Specify **receiver behavior**: tolerate out-of-order UDP; **drop stale video** by frame/slice deadline; **deliver control in strict sequence** per control stream with bounded reorder buffers.
- Specify **FEC** for control and video (e.g., Reed–Solomon or XOR stripes over small code groups) to avoid NACK RTT on critical control.
- Outline **zero-copy ingress/egress** with hardware encoders (NVENC/V4L2 export buffers) into send buffers and decoder-side DMA-friendly regions.
- Expose a **versioned embedder SDK** so control stacks and commercial SDKs link against a **stable C ABI** (and generated bindings) rather than internal Rust-only types.

**Non-Goals:**

- WebRTC, TCP, or “heavy” commercial streaming protocols on the realtime path.
- Full congestion control research (beyond minimal fairness hooks); initial design may use fixed rate + path metrics.
- Complete ROS2 node graph or DDS replacement; only the **bridge contract** and **shmem layout** for the hot path.
- Shipping production-ready plugins for every third-party product; the design targets **documented integration patterns** and **reference adapters** where maintenance is sustainable.

## Decisions

1. **Transport: UDP first, QUIC unreliable optional**  
   - **Choice**: Default to **raw UDP** per path with a single application framing layer; optionally map the same frames to **QUIC datagrams** where middleboxes block UDP or single-path policy requires it.  
   - **Rationale**: Minimal kernel path; full control over scheduling and bonding. QUIC datagrams add crypto and connection state—use only where needed.  
   - **Alternative**: SCTP multipath—rejected for deployment friction and kernel variance.

2. **Sequence and time domains**  
   - **Choice**: Separate **16- or 32-bit sequence spaces per `stream_id`** (control uplink, control downlink, video L/R, telemetry, meta). Global **path_tag** on each datagram for bonding dedup.  
   - **Rationale**: Avoid HOL blocking between streams; simplify “control ordered, video best-effort.”  
   - **Alternative**: Single sequence—rejected due to cross-traffic blocking.

3. **Multi-path bonding modes**  
   - **Choice**:  
     - **Duplicate mode** (default for control + small haptic): replicate datagrams on N interfaces within a time window; receiver takes **first arrival**, discards duplicates by `(path_tag, stream_id, seq)`.  
     - **Split mode** (video slices, large telemetry): **round-robin or weighted** by instantaneous path RTT/loss; **no cross-path ordering assumption**; video reassembly keyed by `(frame_id, slice_id)`.  
     - **Fast switch**: if path A loss/RTT exceeds threshold, **drain** duplicate mode to surviving path; split mode shifts weights within one RTT bucket.  
   - **Rationale**: Duplication minimizes tail latency for small critical packets; splitting maximizes throughput for bursty video.  
   - **Alternative**: Always duplicate—rejected (bandwidth); always split—rejected (control jitter).

4. **Sender priority and multiplexing**  
   - **Choice**: **Strict priority queues** by `PriorityClass` (Control &gt; Haptic &gt; Video &gt; Telemetry). Within video, **earliest-deadline slice first** (slice timestamp + frame id). Scheduler **never** pulls from lower priority if higher has data, except optional **token bucket** to prevent starvation of telemetry (low rate).  
   - **Rationale**: Meets “dropped video never blocks control.”

5. **Receiver: video vs control**  
   - **Choice**: **Control streams**: reorder buffer sized by `max_reorder` (e.g., 2–8 packets); deliver in order to the application callback. **Video**: **frame/slice deadline**; incomplete frames past deadline **discarded**; display **last complete frame** (teleop-typical).  
   - **Rationale**: Predictable operator loop; no blocking on late slices.

6. **FEC**  
   - **Choice**: **Small-N, low-latency codes**: e.g., **(k, n) Reed–Solomon** over a **slice group** for video; **XOR parity** or **(k, n)** over control **micro-batches** (e.g., every 4 control packets + 1 parity). **No NACK** on control path; optional **forward-only** repair for video.  
   - **Rationale**: Bounded CPU; avoids RTT for repair.  
   - **Alternative**: Fountain codes—deferred (complexity).

7. **Implementation language**  
   - **Choice**: **Rust** for the reference stack (socket manager, framer, queues, FEC); **C++20** acceptable for encoder glue if vendor SDKs demand it, behind a narrow FFI.  
   - **Rationale**: Safety + performance + single binary.

8. **ROS2 integration**  
   - **Choice**: **Shared-memory ring** (e.g., lock-free SPSC/MPSC) or **iceoryx-style** zero-copy segments for control and compressed video frames; **optional** ROS2 nodes only for discovery/config; **no DDS** on the hot control/video path.  
   - **Rationale**: Meets “bypass standard DDS overhead” for the loop.

9. **Stable SDK boundary for ecosystems**  
   - **Choice**: Publish a **C ABI** (`ltp_session_*`, `ltp_send`, `ltp_recv`, buffer callbacks) as the **only** long-term stable contract; Rust crate remains the reference implementation but **semantic versioning** applies to the C header. Generate **Python** (ctypes/pyo3) and **optional C++** headers from the same ABI.  
   - **Rationale**: ROS 2 nodes, **Unity/Unreal** (P/Invoke / native plugin), **Isaac Sim / Omniverse**-style extensions, **MATLAB/Simulink** mex, and vendor arms SDKs overwhelmingly integrate via C or C++.  
   - **Alternative**: Rust-only public API—rejected for ecosystem reach.

10. **Payload extension without breaking wire**  
   - **Choice**: **Typed control envelope**: fixed header + `control_schema_id` + `payload_len` + opaque bytes; document **reference schemas** (e.g., joint velocity, twist, gamepad bitmask) in a **versioned registry** (JSON or `.proto` for documentation only—on-wire remains custom framing). Third parties register new `control_schema_id` values without changing LTP header layout.  
   - **Rationale**: Lets MoveIt / custom manipulators / game engines map their native types without forking the protocol.

11. **Ecosystem adapters (reference scope)**  
   - **Choice**: Ship **thin adapters** that are thin wrappers over the C API: **ROS 2** (rclcpp/rclpy bridge + shmem), optional **ROS** `rosbridge`-style or `roscpp` shim for legacy; **document** patterns for **Isaac Sim** (Python extension calling the shared library) and **game engines** (single native plugin module). **MoveIt 2** integration is documented as “subscribe to trajectory / jog topics → map to `control_schema_id`”—not necessarily a first-party MoveIt plugin in v1.  
   - **Rationale**: Maximizes “easy integration” with maintainable scope; heavy frameworks integrate through one ABI.

## Packet header (reference layout)

Fixed **network byte order (big-endian)** for multi-byte fields. Alignment: natural 4-byte where noted; implement packed structs with explicit padding.

| Offset | Size | Field |
|--------|------|--------|
| 0 | 4 | `magic` = `0x4C545031` (`LTP1`) |
| 4 | 1 | `version` (4 bits) \| `hdr_len_dw` (4 bits) — header length in 4-byte words |
| 5 | 1 | `priority` (enum: Control=0, Haptic=1, Video=2, Telemetry=3) |
| 6 | 1 | `payload_type` (discriminant: control, haptic, video_slice, telemetry, fec_shard, ack_meta—ack optional/non-hot) |
| 7 | 1 | `flags` (bit0 FEC present, bit1 keyframe, bit2 duplicate send, bit3–7 reserved) |
| 8 | 2 | `stream_id` (per logical stream for sequence scope) |
| 10 | 2 | `path_tag` (sender path id + generation for dedup) |
| 12 | 4 | `seq` (sequence number for this stream_id) |
| 16 | 8 | `timestamp_tsc` or `capture_ts_ns` (monotonic or UTC-derived per deployment policy) |
| 24 | 2 | `frame_id` (video; 0 if N/A) |
| 26 | 2 | `slice_id` / `slice_count` (packed nibble each if needed, or separate bytes in ext) |
| 28 | 2 | `payload_len` |
| 30 | 2 | `fec_group_id` + `fec_index` (or move to extension) |
| 32 | variable | Optional **extension** (FEC matrix id, codec params hash, stereo eye id) |
| … | … | **Payload** follows immediately |

**MTU strategy**: PMTU discovery per path; **video slices** sized to **path MTU − header − FEC**; **no IP fragmentation** on the hot path.

## Multi-path bonding (receiver reconstruction)

- **Ingress**: Each UDP receive demuxes by `magic`/`version`, validates `hdr_len`, then keys **dedup** as `(path_tag low bits, stream_id, seq)` for duplicate mode.  
- **Ordering**: Per `stream_id`, maintain **next expected seq** + **reorder heap**; size capped.  
- **Video**: Reassembly **hash map** `(frame_id, slice_id) → buffer`; on **frame complete** or **deadline timer**, deliver or drop.  
- **Metrics**: Per-path RTT (timestamps), loss rate, and **EWMA** for scheduler weights.

## Sender priority queue (logic)

- Enqueue by `(priority, stream_id)`; dequeue **strict** from highest priority non-empty queue.  
- **Video slices** carry **deadline**; within Video queue, **earliest deadline first**.  
- **Credit**: Optional per-path **byte credit** to avoid one path overrun in split mode.

## Receiver loop (core behavior)

1. **Recv batch** from all sockets (epoll/kqueue/`io_uring`).  
2. **Parse** → **dedup** → **route** to stream state machine.  
3. **Control/Haptic**: reorder → **ordered callback** (same thread or bounded worker).  
4. **Video**: reassemble / FEC decode → **deadline filter** → decoder feed.  
5. **Telemetry**: best-effort, large reorder buffer optional.

## Zero-copy pipeline (integration)

- **Encoder**: NVENC/V4L2 **export** dmabuf or CUDA device pointers; **register** with send path as **external buffers** or **copy once** into **pinned** ring slots tagged with `frame_id`/`slice_id`.  
- **Socket**: **sendmsg** with **MSG_ZEROCOPY** where kernel + NIC support; fallback to **pinned** memcpy-minimal path.  
- **Decoder**: Reverse—demux delivers **NAL/slice bytes** into **decoder-owned** buffer pool; avoid extra copies on Jetson via **NVDEC** surface import.

## SDK and prominent control-system integration

- **Single entry point**: All first-party bridges (ROS 2, examples) call the **same** session API exported from the shared library; no duplicate framing logic in adapters.  
- **Threading model**: Document which calls are **thread-safe**, which require **single I/O thread**, and how **callbacks** interact with ROS executor / game engine main thread (e.g., lock-free handoff to worker).  
- **Discovery/config**: Session parameters (paths, MTU, FEC, peer addresses) load from **CLI, env, or ROS params**—wire format stays identical regardless of host.  
- **Documentation deliverable**: **Integration guide** listing supported patterns: ROS 2 Jazzy/Humble-style bridge, Isaac Sim Python loading `.so`, Unity `DllImport`, Unreal `FPlatformProcess::GetDllHandle`, bare C++ arms controllers.  
- **License-friendly linking**: Build artifacts suitable for **static or dynamic** link in commercial SDKs (LGPL vs static linking policy documented in repo—not in this design’s normative spec).

## Risks / Trade-offs

- **[Risk] FEC CPU on embedded** → **Mitigation**: small k/n, SIMD RS, or XOR-only for control.  
- **[Risk] Duplicate mode bandwidth** → **Mitigation**: apply only to small control/haptic packets; cap duplicate window.  
- **[Risk] QUIC vs UDP deployment** → **Mitigation**: feature flag; same frame format on both.  
- **[Risk] Sub-50 ms not achievable on non-fiber or congested paths** → **Mitigation**: metrics, adaptive frame/slice sizing; explicit non-goal for worst-case internet.

## Migration Plan

1. Land **framing + mock paths** in-process tests.  
2. Add **multi-path** and **priority scheduler** behind flags.  
3. Add **FEC**, **C ABI + bindings**, **ROS2/shmem bridge**, and **integration guide** as optional crates/features.  
4. Roll out on lab 5G+Wi-Fi before production overseas links.

## Open Questions

- Exact **FEC (k,n)** and **field placement** for smallest header on control packets.  
- Whether **QUIC** is mandatory for any production segment or optional throughout.  
- **Clock sync** between operator and robot (PTP vs monotonic delta only).

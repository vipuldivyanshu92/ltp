## Why

Human-in-the-loop teleoperation over cross-country or overseas links needs a purpose-built transport: generic stacks (WebRTC, TCP, heavy streaming) add latency, head-of-line blocking, and unsuitable reliability models for sub-50 ms glass-to-glass targets where fiber permits. A proprietary UDP/QUIC-unreliable stack with multi-path bonding, strict prioritization, FEC instead of NACK-driven retransmit, and zero-copy ties to hardware codecs is required now to anchor a robots-as-a-service platform.

## What Changes

- Introduce a **custom teleoperation transport** built on raw UDP or QUIC unreliable datagrams (no WebRTC, no TCP, no standard heavy streaming protocols for the realtime path).
- Define **byte-level packet framing** with explicit priority, payload typing, sequence spaces, and timestamps suitable for stereoscopic RGB, IMU/telemetry, and control/force-feedback channels.
- Specify **multi-path bonding** (e.g., 5G, LTE, Wi-Fi): policies for duplication vs split, fast switching under asymmetry, and efficient receiver-side deduplication and ordering.
- Specify **strict sender prioritization** so control and haptic/force-feedback traffic are never blocked by video or bulk telemetry; integrate with **slice-based H.265** (partial-frame egress before full-frame encode completes).
- Specify **FEC** for video and control paths; avoid NACK-driven retransmit on the critical control plane over long RTT.
- Target a **Rust (or C++20) reference implementation** producing a **lightweight binary (&lt;30 MB)** with a **ROS2 integration path** via shared memory or a custom bridge that bypasses default DDS hot paths for the realtime loop.
- Provide a **stable SDK surface** (C ABI first, optional bindings) so the same wire protocol embeds cleanly in **prominent robotics and simulation stacks** (e.g., ROS 2, ROS 1 bridges where needed, NVIDIA Isaac / Isaac Sim–class workflows, MoveIt 2 consumers, Unity/Unreal teleop clients) and **vendor SDKs** without forking framing.
- Document **zero-copy pipeline** strategy from V4L2/NVENC-class sources into the socket send path (Phase 3 integration).

## Capabilities

### New Capabilities

- `teleop-datagram`: Custom UDP/QUIC-unreliable datagram header layout (bitfields, sequence spaces, payload types, priority flags, timestamps) and framing rules.
- `multi-path-bonding`: Multi-interface bonding: packet duplication vs splitting, path health, receiver reconstruction and deduplication with low jitter for the control loop.
- `transport-scheduling`: Sender priority queues and multiplexing; receiver loop for out-of-order datagrams, late video discard, ordered execution of control signals.
- `fec`: Forward error correction schemes and parameters for video slices and control datagrams without relying on NACK latency for critical control.
- `ros2-integration`: Deployment profile (Rust preferred), binary size budget, and ROS2 interop via shared memory or a thin zero-latency bridge avoiding DDS on the hot path.
- `sdk-ecosystem-integration`: Stable embedder API (C ABI, versioning), payload extension points, and documented integration patterns for major control/simulation ecosystems and third-party SDKs.

### Modified Capabilities

<!-- No existing specs under openspec/specs/; none. -->

## Impact

- New protocol modules and binaries in this repository; optional ROS2 bridge package; **C ABI and language bindings** for non-Rust hosts; example or reference adapters where feasible.
- Operational impact: requires multi-interface hosts (robot and operator stations), firewall/UDP considerations, and codec capabilities (e.g., Jetson NVENC/NVDEC, slice mode H.265).
- No change to unrelated subsystems until implementation tasks land.

## Context
Adamo HQ's stack is currently the leading proprietary solution for teleoperation. This protocol is designed to exceed its performance, targeting cross-country and transatlantic networks with ≤ 40 ms glass-to-glass latency, zero-copy serialization, and zero-drift temporal synchronization of video and robotic state.

## Goals / Non-Goals
**Goals:**
* Achieve ≤ 40 ms glass-to-glass latency on good links, gracefully degrading up to 80-120 ms on worst-case transatlantic links.
* Provide multi-path bonding across 5G, LTE, and Wi-Fi seamlessly.
* Implement strict QoS packet scheduling where control/telemetry takes precedence over video.
* Provide sub-10 ms link failover.
* Achieve < 5% CPU overhead on Jetson Orin with 1080p60.
* Integrate natively with ROS 2.

**Non-Goals:**
* General-purpose web media streaming (e.g., Twitch-style HLS).
* Full autonomy stack (this is strictly teleop/transport).

## Decisions
* **Implementation Language (Rust):** Chosen for memory safety, concurrency model, and C-interoperability. Rust's zero-cost abstractions allow us to hit the < 5% CPU overhead target while preventing data races in the multi-path bonding layer. C++ is an alternative, but Rust provides a better safety-to-performance ratio for modern network stacks.
* **Transport Layer (RTP over QUIC/RoQ instead of raw RTP/UDP):** QUIC provides 0-RTT handshakes, built-in encryption (TLS 1.3), stream multiplexing (avoiding Head-of-Line blocking for separate sensor streams), and a standard multipath extension.
* **Serialization (FlatBuffers):** FlatBuffers allows zero-copy access to embedded robot state metadata compared to Protobuf, saving crucial microseconds during processing and packetization.

## Architecture & Reference Implementation Plan
```text
[ Robot Hardware & Sensors ]
           │ (V4L2, ROS topics)
           ▼
┌──────────────────────────────┐
│  Robot Agent (Rust / C++)    │
│  ├─ Zero-Copy Capture        │
│  ├─ NVENC Hardware Encode    │
│  ├─ Metadata Embedder        │
│  ├─ Priority Scheduler (QoS) │
│  └─ Multipath Sender (QUIC)  │
└───────────┬──────────────────┘
            │  Bonded Links (LTE/5G/Wi-Fi)
            ▼
[ Global Cloud / Direct IP ]
            │
            ▼
┌──────────────────────────────┐
│  Operator Client             │
│  ├─ Multipath Receiver       │
│  ├─ Jitter Buffer / Decoder  │
│  ├─ HW Decode (NVDEC/Metal)  │
│  └─ Renderer & HUD           │
└───────────┬──────────────────┘
            │
            ▼
[ Operator Display & Controllers ]
```

### Core Modules (Phase 1)
1. **Agent:** Rust-based zero-copy pipeline extracting frames and telemetry.
2. **Operator Client:** Native Rust/WGPU viewer to minimize display latency.
3. **ROS 2 Bridge:** In-process rclrs node to subscribe/publish states natively.

### Key Code Skeletons

**Packet Builder & Metadata Embedder (Rust)**
```rust
// Embedded payload structure representing a video frame + telemetry
#[repr(packet)]
struct TeleopFrame {
    rtp_header: RtpHeader,
    timestamp_us: u64,
    telemetry: FlatBuffer<RobotState>, // Zero-copy view
    video_nal: [u8], // H.265 NAL unit
}

fn build_packet(frame: H265Frame, state: &RobotState) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let state_offset = serialize_state(&mut builder, state);
    
    // Combine RTP header, serialized state, and video frame
    // (Pseudocode for zero-copy transmission pipeline)
    assemble_roq_datagram(frame.timestamp, state_offset, frame.data)
}
```

**Priority Scheduler (Rust)**
```rust
enum PacketPriority { Control = 0, Telemetry = 1, VideoKeyframe = 2, VideoDelta = 3 }

struct PriorityQueue {
    queues: [VecDeque<Packet>; 4],
}

impl PriorityQueue {
    fn pop_next_to_send(&mut self) -> Option<Packet> {
        for queue in self.queues.iter_mut() {
            if let Some(packet) = queue.pop_front() {
                return Some(packet);
            }
        }
        None
    }
}
```

## ROS 2 Integration
* **Nodes:** A single unified `teleop_agent` node implemented in `rclcpp` or `rclrs`.
* **Topics & Services:**
  * Subscribes to: `/joint_states`, `/imu/data`, `/camera/image_raw` (via `image_transport` / NVMM zero-copy).
  * Publishes to: `/cmd_vel`, `/joint_commands`.
* **Timestamp Sync:** Uses `rclcpp::Clock` synced with NTP/PTP. Video frame hardware timestamps are extracted from V4L2 directly and injected into the QUIC packet metadata alongside the ROS payload.

## Deployment & Testing
* **Sub-40 ms Strategy:** Achieved by removing standard OS network stacks where possible (using io_uring or DPDK on Linux edge), completely disabling TCP wait, configuring NVENC with `lowDelay` preset, tuning Linux scheduler (`SCHED_FIFO`), and disabling all driver-level frame queueing.
* **Benchmarking (Glass-to-Glass):** A high-speed camera (e.g., 240fps) records a physical LED on the robot triggering simultaneously with an on-screen visual indicator on the Operator Client. Latency is calculated by counting frames between the two events.
* **Safety Mechanisms:** 
  * Watchdog timer explicitly bound to the control packet stream. If no control packet with `PacketPriority::Control` is received within 50 ms, the robot immediately executes a soft-stop.
  * Local autonomy fallback to slowly return to a safe resting state if the connection drops.

## Future Extensions
* 1000 Hz real-time control loops for highly dynamic manipulation.
* Bilateral Haptics via dedicated low-latency QUIC datagram streams.
* 8K multi-camera streaming with foveated compression (sending high-res only where the operator is looking).

## Risks / Trade-offs
* **[QUIC Multipath Maturity]** → *Mitigation*: QUIC multipath is still an evolving draft. If unstable, we will implement custom UDP bonding underneath standard RTP.
* **[Cross-Platform Hardware Decoding]** → *Mitigation*: Fall back to WebAssembly/WebCodecs for browser clients, accepting a ~10-15ms penalty for portability.

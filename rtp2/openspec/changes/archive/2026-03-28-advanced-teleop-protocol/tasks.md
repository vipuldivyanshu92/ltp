## 1. Infrastructure and CI/CD Setup

- [x] 1.1 Scaffold project repositories (Agent, Client, Common Protocol Library)
- [x] 1.2 Setup Rust toolchains and build environments for Jetson Orin and Desktop
- [x] 1.3 Configure automated testing for QUIC transport and serialization schemas

## 2. Core Protocol Library (Common)

- [x] 2.1 Define FlatBuffers schemas for `RobotState` and complete metadata layout
- [x] 2.2 Implement QUIC datagram wrapper (RoQ) with 0-RTT handshake support
- [x] 2.3 Implement packet priority queue structures
- [x] 2.4 Build multipath bonding logic for concurrent UDP sockets
- [x] 2.5 Integrate AES-256 TLS 1.3 encryption natively into the transport layer

## 3. Robot Agent Component

- [x] 3.1 Setup V4L2 zero-copy hardware capture loop
- [x] 3.2 Initialize NVENC hardware encoder with low-delay preset
- [x] 3.3 Implement ROS 2 (`rclrs` or `rclcpp`) node to subscribe to topics (`/joint_states`, `/imu/data`)
- [x] 3.4 Build metadata embedder that pairs ROS 2 telemetry with V4L2 timestamps
- [x] 3.5 Assemble final packet buffer and dispatch via QUIC multipath sender
- [x] 3.6 Implement watchdog safety mechanism that triggers local autonomy on timeout

## 4. Operator Client Component

- [x] 4.1 Scaffold native Rust/WGPU application window and rendering surface
- [x] 4.2 Initialize QUIC multipath receiver and jitter-buffer logic
- [x] 4.3 Extract telemetry FlatBuffers payload and pass to UI HUD state
- [x] 4.4 Feed H.265 NAL units to hardware decoder (NVDEC/Metal) with 0-frame queuing
- [x] 4.5 Render decoded video frames synchronously with telemetry data overlays
- [x] 4.6 Develop WebAssembly/WebTransport + WebCodecs fallback implementation

## 5. End-to-End Testing & Benchmarking

- [x] 5.1 Perform initial single-link local loopback testing for functional verification
- [x] 5.2 Test multipath failover by physically disconnecting Wi-Fi/LTE interfaces
- [x] 5.3 Setup LED-to-Screen glass-to-glass high-speed camera benchmark test
- [x] 5.4 Profile Jetson CPU usage to confirm < 5% overhead during 1080p60 transmission

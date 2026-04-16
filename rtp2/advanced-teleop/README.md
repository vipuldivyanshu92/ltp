# Remote Teleoperation via QUIC

Three Rust binaries that enable remote robot teleoperation over the internet
using QUIC (encrypted, congestion-controlled) transport.

## Architecture

```
Robot LAN                          Cloud (VPS)                    Operator (anywhere)
┌──────────────┐                ┌──────────────┐              ┌──────────────┐
│anvil_streamer│─WS 9191─►┌────┤ cloud-relay  │◄────QUIC────┤ quest-proxy  │◄─WS 9192─┐
│  (4 cameras) │          │    │  :4433 QUIC  │              │ :9192 WS     │          │
└──────────────┘    ┌─────┤    └──────────────┘              │ :8082 TCP    │   ┌──────┴──────┐
                    │robot│                                   └──────────────┘   │ Quest VR    │
┌──────────────┐    │relay│                                                      │ Client      │
│quest_teleop  │◄───┤     │    Video: robot→cloud→quest (QUIC uni-streams)      │ (unchanged) │
│ (ROS bridge) │TCP │     │    Control: quest→cloud→robot (QUIC bidi-stream)    └─────────────┘
│  :8081       │8081└─────┘
└──────────────┘
```

## Quick Start

### 1. Build all binaries

```bash
cd advanced-teleop
cargo build --release -p cloud-relay -p robot-relay -p quest-proxy
```

Binaries will be in `target/release/`.

### 2. Deploy cloud-relay (on a VPS)

```bash
# Self-signed cert (auto-generated):
./cloud-relay --port 4433

# Or with your own certs:
./cloud-relay --port 4433 --cert /path/to/cert.pem --key /path/to/key.pem
```

Ensure UDP port 4433 is open in your firewall.

### 3. Run robot-relay (on the robot's LAN)

```bash
./robot-relay \
  --ws-host 127.0.0.1 --ws-port 9191 \
  --cloud-host <VPS_PUBLIC_IP> --cloud-port 4433 \
  --teleop-tcp 127.0.0.1:8081 \
  --video-fps 15
```

This subscribes to the 4 default camera topics on anvil_streamer, forwards
JPEGs to the cloud, and routes incoming control packets to the local
quest_teleop ROS bridge.

### 4. Run quest-proxy (on operator's machine)

```bash
./quest-proxy \
  --cloud-host <VPS_PUBLIC_IP> --cloud-port 4433 \
  --ws-port 9192 --teleop-port 8082
```

### 5. Connect the Quest headset

The Quest app uses its existing **local mode** (WebSocket + TCP). Set up
ADB reverse tunnels so the Quest can reach the proxy:

```bash
adb reverse tcp:9191 tcp:9192   # video (Quest connects to 9191, maps to quest-proxy's 9192)
adb reverse tcp:8081 tcp:8082   # teleop control (Quest connects to 8081, maps to quest-proxy's 8082)
```

Launch the Quest app — it will connect to `127.0.0.1:9191` for video and
`127.0.0.1:8081` for sending controller data, just like local operation.
The `adb reverse` tunnels map these to quest-proxy's ports (9192 / 8082).

> **Single-machine testing:** robot-relay and quest-proxy can run on the
> same machine without port conflicts because they use different default
> ports (robot-relay connects to WS:9191 / TCP:8081; quest-proxy listens
> on WS:9192 / TCP:8082).

## Wire Protocol

### Video (QUIC unidirectional streams)

Each JPEG frame is sent as a separate QUIC unidirectional stream:

```
Offset  Size  Field
0       1     cam_index (0-3)
1       1     flags (reserved)
2       2     frame_id (big-endian u16, wrapping)
4       4     jpeg_len (big-endian u32)
8..     N     JPEG payload
```

Using per-frame streams avoids cross-frame head-of-line blocking while
still providing reliable delivery within each frame.

### Control (QUIC bidirectional stream)

Continuous stream of 112-byte `TelemetryPacket` records (same binary
layout as the existing TCP :8081 protocol):

```
Offset  Size  Field
0       8     timestamp (u64)
8       52    right ControllerData
60      52    left ControllerData
```

## Environment Variables

- `RUST_LOG=info` — default log level; use `debug` or `trace` for more detail

## Notes

- The cloud relay uses **self-signed certificates** by default. Both
  robot-relay and quest-proxy skip server cert verification for development.
  For production, provide proper TLS certificates.
- QUIC's built-in congestion control prevents flooding the network link.
- Connection migration allows the operator to switch WiFi/cellular without
  dropping the session.

Camera → WS → robot-relay:     ~1ms (local)
robot-relay → cloud-relay:     ~RTT/2 (network upload)
cloud-relay streaming:         ~0ms (pipe-through)
cloud-relay → quest-proxy:     ~RTT/2 (network download)
quest-proxy → Quest WS:       ~1ms (adb reverse)
Quest JPEG decode:             ~3-5ms
────────────────────────────────────
Total:                         ~RTT + 5ms


cargo build --release --target x86_64-unknown-linux-musl -p cloud-relay -p robot-relay -p quest-proxy

./robot-relay --cloud-host <VPS_IP> --video-fps 0

adb reverse tcp:9191 tcp:9192   # video
adb reverse tcp:8081 tcp:8082   # control

./quest-proxy --cloud-host <VPS_IP>


scp -i "West-coast-key.pem" /Users/vipuldivyanshu/workspace/axiom/openarm-custom/cloud-relay ubuntu@ec2-54-153-43-113.us-west-1.compute.amazonaws.com:/home/ubuntu/

cp /Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/target/x86_64-unknown-linux-musl/release/cloud-relay /Users/vipuldivyanshu/workspace/axiom/openarm-custom

cp /Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/target/x86_64-unknown-linux-musl/release/robot-relay /Users/vipuldivyanshu/workspace/axiom/openarm-custom

cp /Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/target/x86_64-unknown-linux-musl/release/quest-proxy /Users/vipuldivyanshu/workspace/axiom/openarm-custom
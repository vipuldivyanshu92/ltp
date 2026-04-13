# Remote Teleoperation via QUIC Cloud Relay

## Background & Problem

**Current local flow:**
```
anvil_streamer (WS :9191, 4 cameras) → Quest VR Client (video grid + OpenXR teleop)
Quest VR Client → TCP :8081 (112-byte TelemetryPacket @ ~90Hz) → quest_teleop ROS node → Robot
```

This only works when the Quest is on the **same LAN** as the robot (via `adb reverse` USB tunneling or local WiFi).

**Goal:** Enable a remote operator (anywhere on the internet) to view robot cameras and send controller data with minimal added latency, using the QUIC-based transport already scaffolded in `rtp2/advanced-teleop/`.

## Architecture Analysis: Why Not Just the rtp1 Approach?

The rtp1 approach already works for remote teleop:
- `ltp-robot-gateway` subscribes to anvil_streamer WS, encodes JPEGs into LTP/UDP datagrams → sends to a Python UDP relay on a VPS
- Quest's `LtpQuestTransport` (C++ UDP) connects to the same relay, receives video, sends 112-byte control envelopes back
- Robot gateway unpacks control → forwards to TCP :8081

**Limitations of rtp1:**
1. **Plain UDP** — no encryption, no congestion control, blocked by many corporate/hotel NATs
2. **Dumb relay** — the Python `ltp_udp_relay.py` is a 2-socket UDP forwarder with no auth, no session management
3. **UDP slicing** — maxes out at 255 slices/frame per LTP v1 header, limiting JPEG size per MTU
4. **No multipath** — single UDP socket, no WiFi+cellular bonding

**rtp2 advantages (QUIC):**
1. **QUIC datagrams** — unreliable delivery (no HoL blocking) but with encryption, 0-RTT reconnect, and NAT traversal via QUIC's connection migration
2. **QUIC streams** — reliable ordered delivery available for control data (guarantees every command arrives)
3. **TLS 1.3** — encrypted by default, no separate auth layer needed
4. **Congestion control** — QUIC has built-in CC, prevents flooding the link
5. **Connection migration** — operator can switch networks (WiFi → cellular) without losing session

## Proposed Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│  ROBOT SIDE (same LAN as robot)                                                 │
│                                                                                 │
│  anvil_streamer ──WS :9191──▶ ┌──────────────────────┐                         │
│  (4 camera topics)            │  robot-relay (Rust)   │                         │
│                               │  • WS client → 9191  │                         │
│  quest_teleop ◀──TCP :8081──  │  • TCP client → 8081  │                         │
│  (ROS bridge)                 │  • QUIC client → cloud│                         │
│                               └──────────────────────┘                         │
│                                         │                                       │
│                                    QUIC datagrams (video)                       │
│                                    QUIC stream (control)                        │
│                                         │                                       │
└─────────────────────────────────────────┼───────────────────────────────────────┘
                                          │ Internet
┌─────────────────────────────────────────┼───────────────────────────────────────┐
│  CLOUD (VPS / Railway / Fly.io)         │                                       │
│                               ┌─────────▼────────────┐                         │
│                               │  cloud-relay (Rust)   │                         │
│                               │  • QUIC server :4433  │                         │
│                               │  • Accepts 2 peers:   │                         │
│                               │    "robot" + "quest"  │                         │
│                               │  • Forwards datagrams │                         │
│                               │    bidirectionally     │                         │
│                               └─────────▲────────────┘                         │
│                                         │                                       │
└─────────────────────────────────────────┼───────────────────────────────────────┘
                                          │ Internet
┌─────────────────────────────────────────┼───────────────────────────────────────┐
│  OPERATOR (anywhere)                    │                                       │
│                               ┌─────────▼────────────┐                         │
│                               │  Quest VR Client      │                         │
│                               │  (existing C++ app)   │──▶ Modified to use     │
│                               │  • QUIC client → cloud│    WebSocket localhost  │
│                               └──────────────────────┘    proxy on-device       │
│                                                                                 │
│  OR: local-proxy (small binary on Quest/laptop)                                │
│      • QUIC client → cloud                                                      │
│      • WS server :9191 (cameras) — Quest connects to this                      │
│      • TCP server :8081 (teleop) — Quest sends 112B here                       │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## User Review Required

> [!IMPORTANT]
> **Quest client strategy**: The Quest app already has two modes:
> 1. **Local mode**: WebSocket :9191 + TCP :8081 (direct to robot)
> 2. **LTP/UDP mode**: `LtpQuestTransport` to a VPS relay (rtp1)
>
> For rtp2/QUIC, we have **two options**:
>
> **Option A — On-device QUIC proxy (Recommended)**: Write a small Rust binary (`quest-proxy`) that runs on the Quest (or on the operator's laptop with `adb reverse`). It connects to the cloud relay via QUIC, then exposes local WS :9191 + TCP :8081 — the Quest app uses its existing local mode unchanged. **Zero Quest C++ changes.**
>
> **Option B — Native QUIC in Quest C++**: Add a QUIC (quinn) C++ wrapper or link the Rust `common` crate as a shared library into the Quest NDK build. More complex, more latency-optimal (one fewer hop), but much harder to build/debug.
>
> **I recommend Option A** because it lets us ship and test end-to-end without touching the Quest APK build at all. The extra localhost hop adds <0.1ms.

> [!WARNING]
> **Cloud deployment**: The cloud relay needs a public IP with UDP port 4433 open. Railway/Fly.io support this but may require specific configuration. We should also support a `--no-tls` mode for development (self-signed certs).

## Proposed Changes

### Component 1: `cloud-relay` (New Rust binary)

A minimal QUIC server that accepts exactly two roles: `robot` and `quest`. Datagrams from one are forwarded to the other. Control stream data is also forwarded bidirectionally.

#### [NEW] [cloud-relay/](file:///Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/cloud-relay/)

New Rust crate within the workspace:

- **`Cargo.toml`** — deps: `quinn`, `tokio`, `rustls`, `rcgen` (self-signed certs), `clap`, `tracing`
- **`src/main.rs`** — QUIC server on `:4433`, accepts connections, identifies role via ALPN (`teleop-robot` / `teleop-quest`), forwards datagrams + stream data between the two peers
- **Key behaviors:**
  - Self-signed cert generation at startup (or load from file)
  - Role identification: robot connects with ALPN `teleop-robot`, quest/proxy with `teleop-quest`
  - Datagram forwarding: when video datagrams arrive from robot, relay to quest (and vice versa for control)
  - Stream forwarding: bidirectional reliable stream bridge (for 112-byte control packets that must not be lost)
  - Stats logging every 5s (datagrams/sec, bytes/sec, connected peers)
  - Graceful reconnection handling (if robot disconnects, buffer nothing, just drop until reconnect)

---

### Component 2: `robot-relay` (New Rust binary)

Runs on the robot LAN. Subscribes to anvil_streamer WS :9191, connects to cloud relay via QUIC, forwards video as datagrams. Receives control from QUIC stream, forwards to TCP :8081.

#### [NEW] [robot-relay/](file:///Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/robot-relay/)

New Rust crate:

- **`Cargo.toml`** — deps: `quinn`, `tokio`, `tungstenite`, `rustls`, `clap`, `tracing`, `bytes`
- **`src/main.rs`** — CLI args: `--ws-host`, `--ws-port`, `--cloud-host`, `--cloud-port`, `--teleop-tcp`, `--topics`
- **Key behaviors:**
  - 4 WS subscriber threads (same pattern as `ltp-robot-gateway`) → JPEG assembly
  - QUIC client connection to cloud relay with ALPN `teleop-robot`
  - Each complete JPEG is sent as a QUIC datagram with a minimal 8-byte header: `[cam_index: u8, reserved: u8, frame_id: u16, jpeg_len: u32]` + JPEG bytes. If JPEG > max_datagram_size, we slice it (like rtp1) or use a QUIC unidirectional stream.
  - Incoming QUIC stream data = 112-byte `TelemetryPacket` → forward to TCP :8081
  - FPS limiter + bandwidth budget (reuse patterns from rtp1 gateway)
  - Stats logging

---

### Component 3: `quest-proxy` (New Rust binary)

Runs on operator's machine. Connects to cloud relay via QUIC. Exposes local WS :9191 (video to Quest) + TCP server :8081 (teleop from Quest).

#### [NEW] [quest-proxy/](file:///Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/quest-proxy/)

New Rust crate:

- **`Cargo.toml`** — deps: `quinn`, `tokio`, `tungstenite`, `rustls`, `clap`, `tracing`
- **`src/main.rs`** — CLI args: `--cloud-host`, `--cloud-port`, `--ws-port`, `--teleop-port`
- **Key behaviors:**
  - QUIC client to cloud relay with ALPN `teleop-quest`
  - QUIC datagram recv → parse `[cam_index, _, frame_id, jpeg_len]` header → route JPEG to correct WS topic subscriber
  - WS server on `:9191` mimicking anvil_streamer protocol: Quest connects, sends topic name as text, server streams binary JPEG chunks for that topic
  - TCP server on `:8081`: accepts Quest's 112-byte TelemetryPacket stream → wraps in framed QUIC stream writes to cloud relay
  - Handles Quest not being connected yet (buffers nothing for video, just drops)

---

### Component 4: Workspace Updates

#### [MODIFY] [Cargo.toml](file:///Users/vipuldivyanshu/workspace/axiom/light-touch-protocol/rtp2/advanced-teleop/Cargo.toml)
Add `cloud-relay`, `robot-relay`, `quest-proxy` to workspace members.

---

### Component 5: Quest App (Minimal changes — Option A)

#### No changes needed to Quest C++ code

The Quest app continues to use its existing **local mode** (`WebsocketReceiver` to `:9191` + `TeleopClient` TCP to `:8081`). The `quest-proxy` binary provides these endpoints locally.

**Operator setup:** Run `quest-proxy --cloud-host <VPS_IP>` on a laptop, then `adb reverse tcp:9191 tcp:9191` + `adb reverse tcp:8081 tcp:8081` to expose the proxy's ports to the Quest over USB.

---

## Data Flow (detailed)

### Video (Robot → Operator)
```
anvil_streamer → WS binary chunks → robot-relay (JPEG assembly)
    → QUIC datagram [8B hdr + JPEG] → cloud-relay
    → QUIC datagram → quest-proxy
    → WS binary → Quest app (existing decode pipeline)
```

### Control (Operator → Robot)
```
Quest app → TCP :8081 (112B TelemetryPacket) → quest-proxy
    → QUIC stream (reliable) → cloud-relay
    → QUIC stream → robot-relay
    → TCP :8081 → quest_teleop ROS node → robot
```

> [!NOTE]
> **Why QUIC datagrams for video, QUIC streams for control?**
> - Video frames are time-sensitive and lossy-tolerant: if a frame is late, drop it. QUIC datagrams provide exactly this — unreliable, unordered, no HoL blocking.
> - Control packets (112B @ ~90Hz) are safety-critical: a dropped packet means the robot may hold a stale pose. QUIC streams guarantee reliable, ordered delivery with minimal added latency (~1 RTT for retransmit).

## Wire Protocol

### Video Datagram Header (8 bytes)
```
Offset  Size  Field
0       1     cam_index (0-3)
1       1     flags (0 = normal, 1 = keyframe marker)
2       2     frame_id (big-endian, wrapping u16)
4       4     jpeg_len (big-endian u32, redundant with datagram size but useful for validation)
8..     N     JPEG payload
```

If `8 + jpeg_len > max_datagram_size` (~65KB on most QUIC implementations):
- Fall back to a **unidirectional QUIC stream** per frame: write the 8-byte header + JPEG bytes, then `finish()` the stream. The receiver reads until EOF.
- This adds ~1 RTT of latency per frame but handles large JPEGs.

### Control Stream Protocol
```
Continuous stream of 112-byte TelemetryPackets (same binary format as TCP :8081).
No framing needed — fixed-size records, receiver reads exactly 112 bytes at a time.
```

## Open Questions

> [!IMPORTANT]
> 1. **Option A vs Option B** — Do you want to go with the proxy approach (zero Quest changes) or native QUIC in the Quest app? I recommend A.
> 2. **Cloud hosting** — Where will the cloud relay run? Railway, Fly.io, a VPS? This affects cert/port configuration.
> 3. **JPEG size vs datagram limit** — QUIC max_datagram_size is typically ~1200 bytes (limited by MTU). This means we'll need to either:
>    - (a) Use QUIC streams for video too (reliable but may add latency under loss), or
>    - (b) Chunk JPEGs into multiple QUIC datagrams with slice reassembly (like rtp1 does for UDP), or
>    - (c) Use a very large max_datagram_size (requires GSO/GRO support and may cause IP fragmentation).
>    
>    **Recommendation:** Use QUIC **unidirectional streams** for video frames. Each frame gets its own short-lived stream. QUIC multiplexes streams independently, so there's no cross-frame HoL blocking. If frame N is delayed by retransmits, frame N+1 on a different stream can still arrive and be displayed. This gives us reliable delivery per-frame with minimal HoL blocking.
>
> 4. **Do you want auth/room codes?** — Should the cloud relay require a shared token to pair robot+quest, or is IP allowlisting sufficient for now?

## Verification Plan

### Automated Tests
1. `cargo build` all three new crates
2. Integration test: spin up cloud-relay, robot-relay (with mock WS server), quest-proxy → verify JPEG roundtrip and control roundtrip
3. Latency measurement: timestamp at robot-relay WS recv, measure at quest-proxy WS send

### Manual Verification
1. Deploy cloud-relay to a VPS
2. Run robot-relay on robot LAN
3. Run quest-proxy on operator laptop
4. `adb reverse` and launch Quest app — verify 4-camera grid displays and controller data reaches ROS

## ADDED Requirements

### Requirement: QUIC Transport Base
The system SHALL use QUIC datagrams as the underlying transport layer for all video, audio, and telemetry data.

#### Scenario: Connection Establishment
- **WHEN** the Operator Client connects to the Robot Agent
- **THEN** a 0-RTT QUIC handshake SHALL be attempted, falling back to 1-RTT if tokens are invalid.

### Requirement: Multi-Path Bonding
The system SHALL support simultaneous transmission across multiple network interfaces (e.g., LTE, 5G, Wi-Fi) using the QUIC multipath extension.

#### Scenario: Link Failover
- **WHEN** the primary Wi-Fi link drops packets for more than 10ms
- **THEN** the protocol SHALL instantly schedule all high-priority packets on the secondary LTE/5G link.

### Requirement: Priority Scheduling
The system SHALL prioritize control commands and robot telemetry over video frames.

#### Scenario: Network Congestion
- **WHEN** the available bandwidth drops below the required bitrate for video+telemetry
- **THEN** the scheduler SHALL drop video frames (Delta frames first, then Keyframes) before dropping any telemetry or control packets.

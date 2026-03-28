## ADDED Requirements

### Requirement: Frame-Accurate Telemetry Embedding
The system SHALL embed structured robot state (joint positions, IMU, etc.) alongside the video payload in the same network packet or synced QUIC stream.

#### Scenario: Receiving Telemetry with Video
- **WHEN** the Operator Client reconstructs a video frame for display
- **THEN** it SHALL have immediate, zero-drift access to the exact robot telemetry sampled at the time of the frame's capture.

### Requirement: Zero-Copy Serialization
The system SHALL serialize telemetry data using a zero-copy format (e.g., FlatBuffers) to minimize CPU overhead.

#### Scenario: Processing Telemetry on Edge
- **WHEN** the Robot Agent embeds the telemetry into the packet
- **THEN** it SHALL write the data directly to the network buffer without intermediate memory allocations.

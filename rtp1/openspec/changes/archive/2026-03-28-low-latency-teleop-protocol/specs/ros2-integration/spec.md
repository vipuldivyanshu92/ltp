## ADDED Requirements

### Requirement: ROS2 bridge uses the stable embedder API

ROS 2–specific packages SHALL use only the documented stable C ABI (or officially supported Rust FFI that mirrors that ABI) for all framing, bonding, FEC, and scheduling; they MUST NOT duplicate LTP wire logic in ROS-only code.

#### Scenario: Same library as other hosts

- **WHEN** a ROS 2 node sends or receives teleoperation traffic
- **THEN** it SHALL call into the same shared core as non-ROS integrators per `sdk-ecosystem-integration`

### Requirement: Lightweight binary footprint target

The reference implementation SHALL be structured so a release binary (or split of core library + minimal binary) targeting robot/operator edge devices stays within a 30 MB budget excluding optional debug symbols and vendor SDKs not under this repository’s control.

#### Scenario: Release build budget

- **WHEN** building the reference transport binary with default release flags for the supported target
- **THEN** the artifact size SHALL be documented and SHALL not exceed 30 MB for the core transport and bridge as defined in tasks

### Requirement: ROS2 hot path avoids DDS for realtime streams

The integration SHALL provide a path for control and compressed video frames between ROS2 ecosystem components and the transport using shared memory or an equivalent zero-copy bridge such that the time-critical loop does not traverse standard DDS serialization for those streams.

#### Scenario: Control bypasses DDS serialize

- **WHEN** an operator control sample is produced for teleoperation
- **THEN** it SHALL be handed to the transport via shared memory or the custom bridge without DDS serialize/deserialize on the hot path

### Requirement: Encoder-friendly buffer contract

The integration SHALL document buffer ownership and lifetime for hardware encoder output (e.g., NVENC/V4L2 export buffers) so that slice metadata can reference pinned regions with minimal copies.

#### Scenario: Documented lifetime

- **WHEN** a video slice is queued for send from a hardware buffer
- **THEN** the API SHALL specify until when the buffer must remain valid and when it is safe to recycle the buffer

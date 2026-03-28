## ADDED Requirements

### Requirement: Datagram framing uses a fixed magic and versioned header

The system SHALL prefix every on-wire datagram with a 32-bit magic value `0x4C545031` and a version field in the first header byte such that receivers can reject unknown or corrupted packets before parsing payload.

#### Scenario: Reject unknown version

- **WHEN** a datagram arrives with a magic mismatch or unsupported `version`
- **THEN** the receiver SHALL discard the datagram without parsing payload or advancing stream state

### Requirement: Header exposes priority, stream scope, and sequence

The system SHALL include in each datagram header: a priority class discriminant, a `stream_id` scoped sequence number `seq`, and a monotonic `timestamp` field suitable for latency measurement and frame/slice deadlines.

#### Scenario: Independent sequence spaces

- **WHEN** two datagrams belong to different `stream_id` values
- **THEN** their `seq` values SHALL be interpreted independently and SHALL NOT impose ordering or blocking across streams

### Requirement: Payload type and length are explicit

The system SHALL include `payload_type` and `payload_len` such that the parser can locate the payload without scanning and can route to the correct handler (control, haptic, video slice, telemetry, FEC shard).

#### Scenario: Bounds check before copy

- **WHEN** `payload_len` plus header size exceeds the received IP/UDP length
- **THEN** the receiver SHALL discard the datagram

### Requirement: No TCP, WebRTC, or standard heavy streaming on the realtime path

The reference realtime transport SHALL use raw UDP or QUIC unreliable datagrams only; it MUST NOT use WebRTC, TCP, or standard heavy streaming protocols for the operator–robot realtime media and control plane defined by this change.

#### Scenario: Control plane avoids TCP

- **WHEN** sending time-critical control or haptic datagrams
- **THEN** the implementation SHALL NOT encapsulate them in TCP or WebRTC for that path

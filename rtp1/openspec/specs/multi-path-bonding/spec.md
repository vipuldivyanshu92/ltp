## Requirements

### Requirement: Sender labels path for deduplication

The system SHALL include a `path_tag` (or equivalent) in each datagram so the receiver can distinguish duplicates sent on multiple interfaces from distinct packets.

#### Scenario: Duplicate suppression

- **WHEN** the same logical packet is sent on two paths with identical `(stream_id, seq)` and duplicate-send policy is active
- **THEN** the receiver SHALL process at most one copy for stream state advancement

### Requirement: Duplicate mode for critical small packets

The system SHALL support a duplicate transmission mode in which selected small, high-priority datagrams are replicated across multiple paths within a bounded time window.

#### Scenario: First arrival wins

- **WHEN** duplicate mode is enabled for a datagram and copies arrive on different paths
- **THEN** the receiver SHALL use the first valid copy for delivery and SHALL discard subsequent duplicates without duplicate delivery to the application

### Requirement: Split mode for high-throughput payloads

The system SHALL support a split mode in which large payloads (e.g., video slices) are assigned to paths by a scheduler using per-path metrics, without assuming in-order arrival across paths.

#### Scenario: Cross-path reordering for video

- **WHEN** split mode assigns slices of a frame to different paths
- **THEN** the receiver SHALL reassemble by `(frame_id, slice_id)` and SHALL NOT block control streams on video slice reordering

### Requirement: Fast switching under path degradation

The system SHALL adjust duplicate vs split policy and per-path weights when a path’s measured loss or RTT exceeds configured thresholds.

#### Scenario: Weight shift on loss spike

- **WHEN** path A’s loss rate crosses above its threshold while path B remains healthy
- **THEN** the sender SHALL reduce traffic assigned to path A and SHALL complete the transition within one configurable RTT bucket

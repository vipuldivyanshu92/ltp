## ADDED Requirements

### Requirement: FEC without NACK on critical control path

The system SHALL provide forward error correction for control-class datagrams such that recovery does not depend on negative acknowledgments over long RTT paths.

#### Scenario: Repair without round-trip

- **WHEN** a control FEC group loses one symbol within the code’s correction capability
- **THEN** the receiver SHALL recover the lost payload using FEC data without sending a NACK for that control packet

### Requirement: Video FEC configurable by slice group

The system SHALL support FEC over groups of video slices (e.g., k data + m parity shards) with metadata in the header or extension to identify FEC group membership.

#### Scenario: Parity shard consumed

- **WHEN** a video slice is lost but the corresponding FEC group has sufficient parity to reconstruct
- **THEN** the receiver SHALL reconstruct or mark the frame recoverable per implementation policy without requesting TCP-style retransmit

### Requirement: FEC overhead is bounded

The implementation SHALL expose configuration for maximum FEC overhead (ratio of parity to data) and SHALL bound CPU time suitable for embedded targets.

#### Scenario: Disable FEC under load

- **WHEN** FEC decode time exceeds a configured budget for a frame interval
- **THEN** the implementation MAY reduce `m` or skip FEC for subsequent groups until metrics recover

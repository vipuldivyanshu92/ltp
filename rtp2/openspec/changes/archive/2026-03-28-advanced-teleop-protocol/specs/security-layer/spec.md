## ADDED Requirements

### Requirement: End-to-End Encryption
The system SHALL encrypt all control, telemetry, and video streams end-to-end using AES-256 (via TLS 1.3 in QUIC).

#### Scenario: Secure Transmission
- **WHEN** data traverses any network (public or private)
- **THEN** middleboxes and unauthorized observers SHALL NOT be able to decrypt the payload.

### Requirement: Forward Secrecy and Post-Quantum Readiness
The system SHALL use forward secrecy for all session keys and be architecturally ready to negotiate post-quantum cryptography (e.g., Kyber) during the handshake.

#### Scenario: Key Compromise Protection
- **WHEN** a session key is compromised
- **THEN** past sessions SHALL remain secure and undecryptable.

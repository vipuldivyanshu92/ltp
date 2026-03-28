## ADDED Requirements

### Requirement: Stable C ABI for embedders

The implementation SHALL expose a documented C API for session lifecycle, configuration, send, and receive paths such that non-Rust hosts (C, C++, C#, Python, game engines, simulation stacks) can integrate without linking the Rust standard library internals directly.

#### Scenario: Dynamic library load from Python

- **WHEN** a Python process loads the released shared library and calls the documented session create and pump functions
- **THEN** it SHALL be able to exchange control and media datagrams without requiring a Rust toolchain at the integrator site

### Requirement: ABI and header versioning

The project SHALL version the C header and shared library ABI and SHALL document compatibility rules (which header major/minor matches which `.so` / `.dll`).

#### Scenario: Mismatched library rejection

- **WHEN** an application built against header version V1 links at runtime to a library reporting incompatible ABI version
- **THEN** session creation SHALL fail with a documented error code rather than corrupting memory

### Requirement: Extensible control payload identification

The on-wire format SHALL support a versioned `control_schema_id` (or equivalent) in the control payload envelope so third-party control systems can carry domain-specific commands without changing the LTP datagram header layout.

#### Scenario: Third-party schema registration

- **WHEN** an integrator defines a new control schema for a proprietary manipulator
- **THEN** they SHALL map it to a registered or experimental `control_schema_id` and SHALL NOT require a fork of the base header bitfield layout

### Requirement: Integration guide for prominent ecosystems

The project SHALL publish an integration guide that describes how to embed the library from at least: ROS 2 (bridge / shmem), a native C++ host, a Python-based simulation workflow (e.g., Isaac Sim–class extension pattern), and a game engine (Unity or Unreal native plugin pattern using the C ABI).

#### Scenario: ROS 2 integrator finds a single API

- **WHEN** a robotics integrator reads the integration guide for ROS 2
- **THEN** they SHALL find explicit reference to the same C session API used by other ecosystems, with threading and executor notes

### Requirement: No duplicate framing in adapters

Ecosystem-specific adapters (including ROS 2 nodes) SHALL NOT reimplement LTP header packing or parsing; they SHALL delegate framing and scheduling to the core library.

#### Scenario: Adapter delegates to core

- **WHEN** the ROS 2 bridge sends a control sample
- **THEN** the bytes on the wire SHALL be produced by the core library’s framer and scheduler, not by an independent ROS-only packet builder

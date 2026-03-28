## Requirements

### Requirement: Strict priority scheduling on send

The sender SHALL dequeue datagrams for transmission using strict priority ordering: Control and haptic classes MUST be scheduled before video slices and ambient telemetry when those higher queues are non-empty, subject only to an optional starvation guard for low-rate telemetry.

#### Scenario: Video does not block control

- **WHEN** the video queue contains data and the control queue contains data
- **THEN** the next transmitted datagram SHALL be taken from the control queue

### Requirement: Ordered delivery for control streams

For each control stream, the receiver SHALL deliver payloads to the application in increasing `seq` order after bounded reordering.

#### Scenario: Reorder buffer bound

- **WHEN** a control datagram arrives with `seq` greater than `next_expected` but within `max_reorder`
- **THEN** the receiver SHALL buffer it and SHALL deliver in order once gaps are filled or policy expires

### Requirement: Video slices are deadline-driven and droppable

The receiver SHALL discard late or incomplete video frame data when a frame or slice deadline is exceeded, and SHALL not block control delivery while dropping video.

#### Scenario: Stale frame discarded

- **WHEN** a complete frame cannot be assembled before its deadline and a newer frame is available or signaled
- **THEN** the receiver SHALL drop the stale partial frame state and SHALL continue processing control datagrams without delay

### Requirement: Out-of-order UDP tolerated per stream

The receiver SHALL accept out-of-order datagrams within configured reorder windows per stream without assuming kernel ordering.

#### Scenario: Video slice reorder within frame

- **WHEN** video slices for the same `frame_id` arrive out of order within the reassembly window
- **THEN** the receiver SHALL reassemble correctly or drop per deadline policy without affecting control stream ordering

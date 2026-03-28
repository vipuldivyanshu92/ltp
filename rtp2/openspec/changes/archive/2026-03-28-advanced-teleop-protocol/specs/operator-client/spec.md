## ADDED Requirements

### Requirement: Native Desktop Execution
The Operator Client SHALL be available as a native C++ or Rust binary to guarantee minimum possible render latency.

#### Scenario: Rendering Native Video
- **WHEN** running on a supported desktop OS (Windows/Linux/macOS)
- **THEN** the client SHALL decode and render video frames utilizing native GPU APIs (DirectX, Vulkan, Metal).

### Requirement: WebAssembly Fallback
The Operator Client SHALL provide a WebAssembly/WebTransport based fallback for browser-based operations.

#### Scenario: Running in Browser
- **WHEN** an operator acccesses the teleop interface via a modern web browser
- **THEN** the client SHALL connect using WebTransport and decode video using WebCodecs with an acceptable latency penalty (< 15ms).

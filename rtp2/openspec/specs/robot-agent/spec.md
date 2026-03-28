## ADDED Requirements

### Requirement: Hardware-Accelerated Capture pipeline
The Robot Agent SHALL utilize hardware acceleration (e.g., NVENC on Jetson) for all video encoding processes.

#### Scenario: High-Resolution Encoding
- **WHEN** capturing 1080p video at 60 FPS
- **THEN** the CPU utilization of the agent SHALL remain below 5% on an NVIDIA Jetson Orin.

### Requirement: ROS 2 Native Bridging
The Robot Agent SHALL natively integrate with ROS 2, subscribing to sensor topics and publishing control commands directly.

#### Scenario: Subscribing to Camera Topics
- **WHEN** connected to a ROS 2 network
- **THEN** the agent SHALL ingest camera images using `image_transport` zero-copy mechanisms if available.

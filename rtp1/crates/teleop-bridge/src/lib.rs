//! Host bridge crate for ROS 2 and other ecosystems.
//!
//! **Do not duplicate LTP framing here.** Production ROS 2 nodes should load `libteleop_transport`
//! (or static link) and call the C API declared in `include/ltp.h`, or use the same symbols via
//! `teleop_transport` from a thin `rclcpp`/`rclpy` wrapper process.
//!
//! This crate exists so CI builds the transport + bridge together and so policy stays explicit:
//! adapters are glue only.

pub use teleop_transport::LTP_MAGIC;

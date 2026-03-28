# Release footprint

## Supported targets (reference)

- `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu` (Jetson-class aarch64 Linux)
- `aarch64-apple-darwin` when building on Apple Silicon hosts

Cross-compile with `rustup target add aarch64-unknown-linux-gnu` plus an appropriate linker/sysroot.

Run on a machine with Rust installed:

```bash
cargo build -p teleop-transport --release
ls -la target/release/libteleop_transport.*
```

Expectations:

- With `strip = true` and `lto = true` (workspace `Cargo.toml`), the `cdylib` should stay **well under 30 MB** on Linux and macOS for aarch64/x86_64.
- ROS 2 or game-engine hosts should measure **their** final binary with the transport linked in; GPU vendor SDKs are out of scope for this budget.

#!/usr/bin/env sh
# Cargo/Rust invoke the linker as: <linker> <args...>
# Zig expects: zig cc <args...> for GNU/Linux targets from a macOS host.
exec zig cc -target x86_64-linux-gnu.2.28 "$@"

# Encoder / socket zero-copy notes

## Buffer ownership

1. **Hardware encoder output** (NVENC, V4L2 export, Jetson NVMM): the memory backing a slice **must remain valid** until `ltp_send_*` (or your flush) completes—same contract as `sendmsg`.
2. Tag each slice with `frame_id`, `slice_id`, `slice_count`, and optional **deadline** via `LtpHeader::set_deadline_extension` (first 8 bytes of extension = deadline in nanoseconds).
3. Prefer **one copy** from encoder bitstream into a **pinned** datagram buffer if `MSG_ZEROCOPY` is unavailable on your kernel/NIC.

## MSG_ZEROCOPY

- Linux: `sendmsg` with `MSG_ZEROCOPY` can avoid copies when buffers are page-aligned and stable for the duration of the async completion. Fall back to a normal send on `EOPNOTSUPP` or embedded targets without support.
- This reference crate uses standard `UdpSocket::send_to`; wire in `sendmsg` via `libc` in a follow-up if you need zero-copy on x86 servers.

## Footprint

- See `docs/FOOTPRINT.md` after `cargo build --release`. The OpenSpec target is **&lt;30 MB** for core + thin bridge excluding vendor GPU SDKs.

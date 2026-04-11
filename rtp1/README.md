Robot (with anvil_streamer on 9191 and quest_teleop on 8081):

cargo build --release -p ltp-robot-gateway
./target/release/ltp-robot-gateway --ltp-peer <VPS_PUBLIC_IP>:6001 --teleop-tcp 127.0.0.1:8081

Through a VPS UDP relay (`python/ltp_udp_relay.py`): robot uses **6001** (LTP leader leg); Quest uses **5001** (LTP follower leg — video in, control out). The relay sends video **out the 5001 socket** so NAT mappings opened by the Quest stay valid; do not use an older relay that forwarded video from source port 6001.

`--teleop-tcp` must be a real remote address (`127.0.0.1:8081` for local `quest_teleop`); `0.0.0.0` is rejected. If gateway logs show a large `pending_send`, reduce camera JPEG size or cautiously raise `--ltp-mtu` so fewer UDP datagrams are needed per frame.

**Quest video:** ship a `teleop_transport` build that includes the `VideoReassembly` clock-skew fix (`receive.rs`); otherwise a headset whose `now_ns` is ahead of the robot can drop every video slice before reassembly.
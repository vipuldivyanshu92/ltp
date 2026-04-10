Robot (with anvil_streamer on 9191 and quest_teleop on 8081):

cargo build --release -p ltp-robot-gateway
./target/release/ltp-robot-gateway --ltp-peer <VPS_PUBLIC_IP>:6001 --teleop-tcp 127.0.0.1:8081
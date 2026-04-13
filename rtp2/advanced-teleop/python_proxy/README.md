# Python QUIC Latency Proxy

This directory contains a standalone Python proxy setup to quickly test the end-to-end QUIC datagram/stream latency across a network, using your existing `lerobot` Python stack.

## Installation

```bash
cd python_proxy
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
```

## Setup & Testing

### 1. West Coast Server (Reflector)
Run this on your West Coast Ubuntu server. It serves as the QUIC relay.
```bash
python3 reflector_server.py
```
*Note: Make sure UDP port `4433` is open on your firewall! QUIC uses UDP, not TCP.*

### 2. Follower Arm (Mac)
Run the local WebSocket-to-QUIC proxy on the terminal connected to your follower arm.
```bash
# Connect to the reflector server
python3 local_proxy.py --quic-host <WEST_COAST_SERVER_IP> --quic-port 4433 --ws-port 8080
```

Then run your `lerobot` follower command, pointing it to the local proxy:
```bash
PYTHONPATH=src python -m lerobot.scripts.lerobot_teleoperate_ws_follower_cam \
    --robot.type=so101_follower \
    --robot.port=/dev/tty.usbmodem5B141113581 \
    --robot. камеры='{front: {type: opencv, index_or_path: 0, width: 640, height: 480, fps: 30}}' \
    --ws_url=ws://localhost:8080
```

### 3. Leader Arm (Mac)
If your leader arm is on the same machine, run a *second* proxy on a different local port (e.g. 8081).
```bash
python3 local_proxy.py --quic-host <WEST_COAST_SERVER_IP> --quic-port 4433 --ws-port 8081
```

Then start the leader script pointing to the *second* proxy:
```bash
PYTHONPATH=src python -m lerobot.scripts.lerobot_teleoperate_ws_leader_cam \
    --teleop.type=so101_leader \
    --teleop.port=/dev/tty.usbmodem5B141112611 \
    --ws_url=ws://localhost:8081
```

Data will now flow: Leader -> WS -> Local Proxy -> QUIC over Internet -> Reflector -> QUIC over Internet -> Local Proxy -> WS -> Follower.

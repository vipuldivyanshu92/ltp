# Copyright 2025 The HuggingFace Inc. team. All rights reserved.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""
Follower-side client: LTP (UDP) actions plus RGB camera streaming to the leader.

Pairs with `lerobot_teleoperate_ltp_leader_cam`. Requires `robot.cameras` (e.g. OpenCV)
for camera preview on the leader. Uses the same JSON payloads as the WebSocket relay:

  - Incoming: UTF-8 JSON `{"action": ..., "timestamp": ...}` (schema 0xE001)
  - Outgoing camera: UTF-8 JSON `{"frames": {name: base64_jpeg}}` (schema 0xE002)

Build `libteleop_transport` (`cargo build -p teleop-transport --release`).
PYTHONPATH must include LeRobot `src` and this repo's `python/` directory.

Example (follower binds on 5000, leader is at 192.168.1.10:6000):

    export PYTHONPATH="src:/path/to/rtp1/python"
    python -m lerobot.scripts.lerobot_teleoperate_ltp_follower_cam \\
        --robot.type=so101_follower \\
        --robot.port=/dev/tty.usbmodem5B141113581 \\
        --robot.id=my_awesome_follower_arm \\
        --robot.cameras='{front: {type: opencv, index_or_path: 0, width: 640, height: 480, fps: 30}}' \\
        --ltp_bind_host=0.0.0.0 \\
        --ltp_bind_port=5000 \\
        --ltp_peer_host=192.168.1.10 \\
        --ltp_peer_port=6000

`stream_action` / `stream_camera` must match the leader defaults (1 and 2) unless you
override both sides.

Paired localhost example: follower `--ltp_bind_port=5000 --ltp_peer_host=127.0.0.1
--ltp_peer_port=6000` with leader `--ltp_bind_port=6000 --ltp_peer_host=127.0.0.1
--ltp_peer_port=5000`.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import time
from collections import deque
from dataclasses import asdict, dataclass
from pprint import pformat
from typing import Any

import cv2

from lerobot.configs import parser
from lerobot.robots import (  # noqa: F401
    RobotConfig,
    make_robot_from_config,
    so_follower,
)
from lerobot.utils.import_utils import register_third_party_plugins
from lerobot.utils.robot_utils import precise_sleep
from lerobot.utils.utils import init_logging, move_cursor_up

from ltp_ctypes import (
    LTP_RECV_KIND_CONTROL,
    LTP_SCHEMA_LEROBOT_CAMERA_JSON,
    LTP_SCHEMA_LEROBOT_TELEOP_JSON,
    LtpSession,
)

logger = logging.getLogger(__name__)

LATENCY_WINDOW = 1000
STATS_INTERVAL = 100


def _latency_stats(latencies_ms: deque) -> dict[str, float]:
    if not latencies_ms:
        return {}
    sorted_ms = sorted(latencies_ms)
    n = len(sorted_ms)
    return {
        "mean": sum(sorted_ms) / n,
        "p50": sorted_ms[int(0.50 * n)] if n > 0 else 0,
        "p95": sorted_ms[int(0.95 * n)] if n > 1 else sorted_ms[0],
        "p99": sorted_ms[int(0.99 * n)] if n > 1 else sorted_ms[0],
    }


def _encode_jpeg_frames_json(robot) -> str | None:
    """RGB frames from cameras -> single JSON text message with base64 JPEG per camera."""
    frames_b64: dict[str, str] = {}
    for cam_key, cam in robot.cameras.items():
        try:
            rgb = cam.read_latest()
        except Exception as e:
            logger.debug("Skipping %s: %s", cam_key, e)
            continue
        if rgb is None or rgb.size == 0:
            continue
        bgr = cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR)
        ok, buf = cv2.imencode(".jpg", bgr, [int(cv2.IMWRITE_JPEG_QUALITY), 85])
        if not ok or buf is None:
            continue
        frames_b64[cam_key] = base64.b64encode(buf.tobytes()).decode("ascii")
    if not frames_b64:
        return None
    return json.dumps({"frames": frames_b64})


@dataclass
class LtpFollowerCamConfig:
    robot: RobotConfig
    ltp_bind_host: str = "0.0.0.0"
    ltp_bind_port: int = 5000
    ltp_peer_host: str = "127.0.0.1"
    ltp_peer_port: int = 6000
    stream_action: int = 1
    stream_camera: int = 2
    fps: int = 60
    camera_fps: int = 15
    latency_log: str | None = None


async def run_follower(cfg: LtpFollowerCamConfig, robot, session: LtpSession, log_file):
    latencies_ms: deque = deque(maxlen=LATENCY_WINDOW)
    msg_count = 0
    last_cam_send = 0.0
    cam_interval = 1.0 / max(cfg.camera_fps, 0.001)
    action_interval = 1.0 / max(cfg.fps, 0.001)

    logger.info(
        "LTP follower: bind %s:%s → peer %s:%s (actions stream %s, camera stream %s)",
        cfg.ltp_bind_host,
        cfg.ltp_bind_port,
        cfg.ltp_peer_host,
        cfg.ltp_peer_port,
        cfg.stream_action,
        cfg.stream_camera,
    )
    if not robot.cameras:
        logger.warning("No cameras configured on robot; only action streaming is active.")

    def tick():
        nonlocal msg_count, last_cam_send
        loop_start = time.perf_counter()
        session.poll_recv(time.time_ns())
        while True:
            item = session.recv_pop_bytes()
            if item is None:
                break
            kind, schema, payload = item
            if kind != LTP_RECV_KIND_CONTROL or schema != LTP_SCHEMA_LEROBOT_TELEOP_JSON:
                continue
            receive_time = time.perf_counter()
            try:
                data = json.loads(payload.decode("utf-8"))
            except json.JSONDecodeError as e:
                logger.warning("Invalid teleop JSON: %s", e)
                continue
            action: dict[str, Any] = data.get("action", {}) or {}
            if not action:
                continue
            timestamp: float = float(data.get("timestamp", 0.0))
            latency_ms = (receive_time - timestamp) * 1000.0
            latencies_ms.append(latency_ms)
            msg_count += 1

            _ = robot.send_action(action)

            if log_file:
                log_file.write(f"{receive_time},{latency_ms}\n")
                log_file.flush()

            if msg_count % STATS_INTERVAL == 0 and latencies_ms:
                stats = _latency_stats(latencies_ms)
                s = (
                    f"Latency: mean={stats['mean']:.2f}ms "
                    f"p50={stats['p50']:.2f}ms p95={stats['p95']:.2f}ms "
                    f"p99={stats['p99']:.2f}ms (n={len(latencies_ms)})"
                )
                print(s)
                move_cursor_up(1)

        now = time.perf_counter()
        if robot.cameras and now - last_cam_send >= cam_interval:
            last_cam_send = now
            try:
                payload = _encode_jpeg_frames_json(robot)
                if payload:
                    session.send_control(
                        cfg.stream_camera,
                        LTP_SCHEMA_LEROBOT_CAMERA_JSON,
                        payload.encode("utf-8"),
                    )
            except Exception as e:
                logger.warning("Camera LTP send failed: %s", e)

        precise_sleep(max(action_interval - (time.perf_counter() - loop_start), 0.0))

    while True:
        await asyncio.to_thread(tick)


@parser.wrap()
def teleoperate_ltp_follower_cam(cfg: LtpFollowerCamConfig):
    init_logging()
    logging.info(pformat(asdict(cfg)))

    robot = make_robot_from_config(cfg.robot)
    robot.connect()

    log_file = None
    if cfg.latency_log == "csv":
        log_file = open("latency_log.csv", "w")
        log_file.write("receive_time,latency_ms\n")

    session = LtpSession(
        cfg.ltp_bind_host,
        cfg.ltp_bind_port,
        cfg.ltp_peer_host,
        cfg.ltp_peer_port,
    )
    try:
        asyncio.run(run_follower(cfg, robot, session, log_file))
    except KeyboardInterrupt:
        pass
    finally:
        session.close()
        robot.disconnect()
        if log_file:
            log_file.close()
            logger.info("Latency log written to latency_log.csv")


def main():
    register_third_party_plugins()
    teleoperate_ltp_follower_cam()


if __name__ == "__main__":
    main()

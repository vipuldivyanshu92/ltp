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
Leader-side client: LTP (UDP) teleop plus live view of follower RGB cameras.

Replaces WebSocket relay URLs with bind/peer host+port. Requires a matching
`lerobot_teleoperate_ltp_follower_cam` (or equivalent) on the follower that:

  - Listens on `ltp_bind_*` / sends to `ltp_peer_*` (inverse of leader)
  - Sends camera updates as UTF-8 JSON in `ltp_send_control` with schema
    `LTP_SCHEMA_LEROBOT_CAMERA_JSON` (0xE002), same shape as the WS relay:
    `{"frames": {"camera_name": "<base64 jpeg>"}}` on `stream_camera`

Install: build `libteleop_transport` (`cargo build -p teleop-transport --release`).

PYTHONPATH must include LeRobot `src` and this repo's `python/` directory, e.g.:

    export PYTHONPATH="src:/path/to/rtp1/python"
    python -m lerobot.scripts.lerobot_teleoperate_ltp_leader_cam \\
        --teleop.type=so101_leader \\
        --teleop.port=/dev/tty.usbmodem5B141112611 \\
        --teleop.id=my_awesome_leader_arm \\
        --ltp_bind_host=0.0.0.0 \\
        --ltp_bind_port=0 \\
        --ltp_peer_host=192.168.1.50 \\
        --ltp_peer_port=5000

Optional: `LTP_LIBRARY_PATH` points at the shared library if not under `target/release/`.

Use `--receive_camera=false` if you only need arm control without a local preview.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import time
from dataclasses import asdict, dataclass
from pprint import pformat

import cv2
import numpy as np

from lerobot.configs import parser
from lerobot.teleoperators import (  # noqa: F401
    TeleoperatorConfig,
    make_teleoperator_from_config,
    so_leader,
)
from lerobot.utils.import_utils import register_third_party_plugins
from lerobot.utils.robot_utils import precise_sleep
from lerobot.utils.utils import init_logging

from ltp_ctypes import (
    LTP_RECV_KIND_CONTROL,
    LTP_RECV_KIND_VIDEO_JPEG,
    LTP_SCHEMA_LEROBOT_CAMERA_JSON,
    LtpSession,
)

logger = logging.getLogger(__name__)


@dataclass
class LtpLeaderCamConfig:
    teleop: TeleoperatorConfig
    ltp_bind_host: str = "0.0.0.0"
    ltp_bind_port: int = 0
    ltp_peer_host: str = "127.0.0.1"
    ltp_peer_port: int = 8765
    stream_action: int = 1
    stream_camera: int = 2
    fps: int = 60
    receive_camera: bool = True
    camera_window_prefix: str = "follower"


def _decode_and_show_camera_json(inner_utf8: bytes, window_prefix: str) -> None:
    text = inner_utf8.decode("utf-8")
    data = json.loads(text)
    frames = data.get("frames", {})
    if not isinstance(frames, dict):
        return
    for name, b64 in frames.items():
        raw = base64.b64decode(b64)
        arr = np.frombuffer(raw, dtype=np.uint8)
        img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
        if img is None:
            continue
        win = f"{window_prefix}_{name}"
        cv2.imshow(win, img)
    cv2.waitKey(1)


def _decode_and_show_jpeg_blob(jpeg: bytes, window_prefix: str) -> None:
    arr = np.frombuffer(jpeg, dtype=np.uint8)
    img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
    if img is None:
        return
    cv2.imshow(f"{window_prefix}_video", img)
    cv2.waitKey(1)


def _drain_camera_queue(session: LtpSession, cfg: LtpLeaderCamConfig) -> None:
    if not cfg.receive_camera:
        while True:
            item = session.recv_pop_bytes()
            if item is None:
                break
        return
    while True:
        item = session.recv_pop_bytes()
        if item is None:
            break
        kind, schema, payload = item
        try:
            if kind == LTP_RECV_KIND_CONTROL and schema == LTP_SCHEMA_LEROBOT_CAMERA_JSON:
                _decode_and_show_camera_json(payload, cfg.camera_window_prefix)
            elif kind == LTP_RECV_KIND_VIDEO_JPEG:
                _decode_and_show_jpeg_blob(payload, cfg.camera_window_prefix)
        except Exception as e:
            logger.warning("Failed to display camera frame: %s", e)


def run_leader_sync(cfg: LtpLeaderCamConfig, teleop, session: LtpSession) -> None:
    """One thread owns the LTP session: poll RX, drain camera queue, send actions."""
    loop_start = time.perf_counter()
    session.poll_recv(time.time_ns())
    _drain_camera_queue(session, cfg)
    raw_action = teleop.get_action()
    session.send_lerobot_action_json(cfg.stream_action, raw_action)
    dt_s = time.perf_counter() - loop_start
    precise_sleep(max(1.0 / cfg.fps - dt_s, 0.0))


async def run_leader(cfg: LtpLeaderCamConfig, teleop, session: LtpSession):
    logger.info(
        "LTP leader: bind %s:%s → peer %s:%s (action stream %s, expect camera on stream %s)",
        cfg.ltp_bind_host,
        cfg.ltp_bind_port,
        cfg.ltp_peer_host,
        cfg.ltp_peer_port,
        cfg.stream_action,
        cfg.stream_camera,
    )
    while True:
        await asyncio.to_thread(run_leader_sync, cfg, teleop, session)


@parser.wrap()
def teleoperate_ltp_leader_cam(cfg: LtpLeaderCamConfig):
    init_logging()
    logging.info(pformat(asdict(cfg)))

    teleop = make_teleoperator_from_config(cfg.teleop)
    teleop.connect()

    session = LtpSession(
        cfg.ltp_bind_host,
        cfg.ltp_bind_port,
        cfg.ltp_peer_host,
        cfg.ltp_peer_port,
    )
    try:
        asyncio.run(run_leader(cfg, teleop, session))
    except KeyboardInterrupt:
        pass
    finally:
        session.close()
        if cfg.receive_camera:
            try:
                cv2.destroyAllWindows()
            except Exception:
                pass
        teleop.disconnect()


def main():
    register_third_party_plugins()
    teleoperate_ltp_leader_cam()


if __name__ == "__main__":
    main()

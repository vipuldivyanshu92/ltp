#!/usr/bin/env python3
"""
Opaque UDP relay for LTP teleop (leader ↔ follower via a public VPS).

LTP datagrams are forwarded as raw bytes; no parsing. Use when both arms are behind
NAT or you want traffic to pin through one region (e.g. US West).

Topology
--------

  Leader (anywhere)  --UDP-->  VPS:PORT_FROM_LEADER  ----forward---->  Follower (anywhere)
  Follower           --UDP-->  VPS:PORT_FROM_FOLLOWER ----forward---->  Leader

On first datagram from each side, the relay learns that side's public (ip, port).
After both are known, each incoming datagram on one port is sent to the other side's
last seen address.

Deploy on a small West Coast VPS (Ubuntu):

  sudo apt update && sudo apt install -y python3
  scp ltp_udp_relay.py user@vps:~/
  python3 ltp_udp_relay.py --bind 0.0.0.0 --from-leader 6001 --from-follower 5001

Open UDP in the cloud firewall/security group for those two ports.

Client config (must match relay ports)
--------------------------------------

Leader::

  --ltp_bind_host=0.0.0.0 --ltp_bind_port=<any local port or 0>
  --ltp_peer_host=<VPS_PUBLIC_IP> --ltp_peer_port=6001

Follower::

  --ltp_bind_host=0.0.0.0 --ltp_bind_port=<any local port or 0>
  --ltp_peer_host=<VPS_PUBLIC_IP> --ltp_peer_port=5001

Send a few packets from each side (run both scripts) so the relay learns both endpoints;
if one side starts late, the other side's traffic is dropped until the first packet arrives.

Latency adds roughly RTT(leader↔VPS) + RTT(follower↔VPS) per direction.

Alternative: Tailscale/WireGuard with both machines on the same tailnet avoids a custom
relay; use the follower's tailnet IP as ltp_peer_host on the leader (and vice versa).
"""

from __future__ import annotations

import argparse
import asyncio
import logging

logger = logging.getLogger(__name__)


class RelayState:
    def __init__(self) -> None:
        self.leader_addr: tuple[str, int] | None = None
        self.follower_addr: tuple[str, int] | None = None


class RelayProtocol(asyncio.DatagramProtocol):
    def __init__(self, state: RelayState, side: str) -> None:
        self.state = state
        self.side = side
        self.transport: asyncio.DatagramTransport | None = None

    def connection_made(self, transport: asyncio.BaseTransport) -> None:
        self.transport = transport  # type: ignore[assignment]

    def datagram_received(self, data: bytes, addr: tuple[str, int]) -> None:
        if self.side == "leader":
            self.state.leader_addr = addr
            dst = self.state.follower_addr
            label = "leader→follower"
        else:
            self.state.follower_addr = addr
            dst = self.state.leader_addr
            label = "follower→leader"

        if dst is None:
            logger.debug("No peer yet; learned %s at %s:%s", self.side, addr[0], addr[1])
            return
        if self.transport is None:
            return
        self.transport.sendto(data, dst)
        logger.debug("%s %d bytes", label, len(data))


async def run_relay(bind: str, from_leader: int, from_follower: int) -> None:
    state = RelayState()
    await asyncio.get_running_loop().create_datagram_endpoint(
        lambda: RelayProtocol(state, "leader"),
        local_addr=(bind, from_leader),
    )
    await asyncio.get_running_loop().create_datagram_endpoint(
        lambda: RelayProtocol(state, "follower"),
        local_addr=(bind, from_follower),
    )
    logger.info(
        "LTP UDP relay on %s: UDP %s (leader leg) / %s (follower leg)",
        bind,
        from_leader,
        from_follower,
    )
    await asyncio.Event().wait()


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    p = argparse.ArgumentParser(description="UDP relay for LTP leader/follower")
    p.add_argument("--bind", default="0.0.0.0", help="Listen address")
    p.add_argument(
        "--from-leader",
        type=int,
        default=6001,
        metavar="PORT",
        help="Port where leader sends (relay forwards to follower)",
    )
    p.add_argument(
        "--from-follower",
        type=int,
        default=5001,
        metavar="PORT",
        help="Port where follower sends (relay forwards to leader)",
    )
    args = p.parse_args()
    try:
        asyncio.run(run_relay(args.bind, args.from_leader, args.from_follower))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

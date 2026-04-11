#!/usr/bin/env python3
"""
Opaque UDP relay for LTP teleop (leader ↔ follower via a public VPS).

LTP datagrams are forwarded as raw bytes; no parsing. Use when both arms are behind
NAT or you want traffic to pin through one region (e.g. US West).

Topology
--------

Canonical VPS ports (defaults on this script):

  6001 — Robot `ltp-robot-gateway` (LTP leader): camera video to relay; control from relay.
  5001 — Quest (LTP follower): video from relay; control to relay.

On the VPS:

  Leader leg (robot)   --UDP-->  VPS:PORT_FROM_LEADER   ----forward---->  Follower (Quest)
  Follower leg (Quest) --UDP-->  VPS:PORT_FROM_FOLLOWER ----forward---->  Leader (robot)

Defaults: PORT_FROM_LEADER=6001, PORT_FROM_FOLLOWER=5001.

On first datagram from each side, the relay learns that side's public (ip, port).
After both are known, each incoming datagram on one port is sent to the other side's
last seen address.

Deploy on a small West Coast VPS (Ubuntu):

  sudo apt update && sudo apt install -y python3
  scp ltp_udp_relay.py user@vps:~/
  python3 ltp_udp_relay.py --bind 0.0.0.0 --from-leader 6001 --from-follower 5001 \\
      --public-ip $(curl -s ifconfig.me)

`--public-ip` only affects log text: robot targets `ADDR:6001`, Quest targets `ADDR:5001`
(this VPS), not the robot's home/public IP.

Open UDP in the cloud firewall/security group for 6001 and 5001.

Client config (must match relay ports)
--------------------------------------

Robot `ltp-robot-gateway` (LTP leader), from `rtp1/`:

  cargo run --release -p ltp-robot-gateway -- --ltp-peer <VPS_PUBLIC_IP>:6001 ...

Quest LTP follower: set peer to `<VPS_PUBLIC_IP>:5001` (local bind port is often ephemeral).

Generic peer ports (if your client uses flag-style config):

  --ltp_peer_host=<VPS_PUBLIC_IP> --ltp_peer_port=6001   # robot → relay
  --ltp_peer_host=<VPS_PUBLIC_IP> --ltp_peer_port=5001   # Quest → relay

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
import time

logger = logging.getLogger(__name__)


class RelayState:
    """Counters are updated from the asyncio loop thread only."""

    def __init__(self, from_follower: int) -> None:
        self.from_follower = from_follower
        self.leader_addr: tuple[str, int] | None = None
        self.follower_addr: tuple[str, int] | None = None
        self._warned_leader_stall = False
        self._warned_follower_stall = False
        self.pkts_in_leader = 0
        self.bytes_in_leader = 0
        self.pkts_in_follower = 0
        self.bytes_in_follower = 0
        self.pkts_fwd_to_follower = 0
        self.bytes_fwd_to_follower = 0
        self.pkts_fwd_to_leader = 0
        self.bytes_fwd_to_leader = 0
        self._started = time.monotonic()
        # Rate-limit reminder when leader is active but Quest never hits the follower port.
        self._next_follower_missing_reminder = 0.0

    def log_periodic(self) -> None:
        age = int(time.monotonic() - self._started)
        now_m = time.monotonic()
        if (
            self.follower_addr is None
            and self.pkts_in_leader > 500
            and now_m >= self._next_follower_missing_reminder
        ):
            self._next_follower_missing_reminder = now_m + 30.0
            logger.warning(
                "relay: leader UDP traffic is high but follower port %d has received 0 packets — "
                "the Quest (follower) LTP client must send UDP to this machine's **public IP** on port **%d** "
                "(same as --from-follower). Until then: no video to Quest, no control to the robot. "
                "Check Quest `ltp_peer` / firewall / outbound UDP.",
                self.from_follower,
                self.from_follower,
            )
        la = f"{self.leader_addr[0]}:{self.leader_addr[1]}" if self.leader_addr else "—"
        fa = f"{self.follower_addr[0]}:{self.follower_addr[1]}" if self.follower_addr else "—"
        logger.info(
            "relay stats (uptime %ds) endpoints leader=%s follower=%s | "
            "in L%d/%dB F%d/%dB | out→F %d/%dB →L %d/%dB",
            age,
            la,
            fa,
            self.pkts_in_leader,
            self.bytes_in_leader,
            self.pkts_in_follower,
            self.bytes_in_follower,
            self.pkts_fwd_to_follower,
            self.bytes_fwd_to_follower,
            self.pkts_fwd_to_leader,
            self.bytes_fwd_to_leader,
        )


class RelayProtocol(asyncio.DatagramProtocol):
    def __init__(self, state: RelayState, side: str) -> None:
        self.state = state
        self.side = side
        self.transport: asyncio.DatagramTransport | None = None

    def connection_made(self, transport: asyncio.BaseTransport) -> None:
        self.transport = transport  # type: ignore[assignment]

    def datagram_received(self, data: bytes, addr: tuple[str, int]) -> None:
        st = self.state
        if self.side == "leader":
            if st.leader_addr != addr:
                logger.info("leader leg: learned UDP endpoint %s:%s", addr[0], addr[1])
            st.leader_addr = addr
            st.pkts_in_leader += 1
            st.bytes_in_leader += len(data)
            dst = st.follower_addr
            label = "leader→follower"
        else:
            if st.follower_addr != addr:
                logger.info(
                    "follower leg: learned UDP endpoint %s:%s (Quest / follower must hit this port)",
                    addr[0],
                    addr[1],
                )
            st.follower_addr = addr
            st.pkts_in_follower += 1
            st.bytes_in_follower += len(data)
            dst = st.leader_addr
            label = "follower→leader"

        if dst is None:
            if self.side == "leader" and not st._warned_leader_stall:
                st._warned_leader_stall = True
                logger.warning(
                    "Leader datagrams received but follower unknown — nothing is forwarded to Quest "
                    "until the follower sends at least one UDP packet to port %s. "
                    "Start the Quest LTP client pointed at this host:%s.",
                    st.from_follower,
                    st.from_follower,
                )
            elif self.side == "follower" and not st._warned_follower_stall:
                st._warned_follower_stall = True
                logger.warning(
                    "Follower datagrams received but leader unknown — control will not reach the "
                    "robot until the leader sends to the relay leader port.",
                )
            return
        if self.transport is None:
            return
        self.transport.sendto(data, dst)
        if self.side == "leader":
            st.pkts_fwd_to_follower += 1
            st.bytes_fwd_to_follower += len(data)
        else:
            st.pkts_fwd_to_leader += 1
            st.bytes_fwd_to_leader += len(data)
        logger.debug("%s %d bytes → %s:%s", label, len(data), dst[0], dst[1])


async def _stats_loop(state: RelayState, interval: float) -> None:
    while True:
        await asyncio.sleep(interval)
        state.log_periodic()


async def run_relay(
    bind: str,
    from_leader: int,
    from_follower: int,
    public_ip: str | None,
) -> None:
    state = RelayState(from_follower)
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
    if public_ip:
        logger.info(
            "Client wiring for this relay — Leader (robot `ltp-robot-gateway`): UDP peer %s:%d",
            public_ip,
            from_leader,
        )
        logger.info(
            "Client wiring for this relay — Follower (Quest LTP): send **all** follower UDP "
            "(control + keepalive) to %s:%d — NOT the robot's public IP; use this VPS address.",
            public_ip,
            from_follower,
        )
    asyncio.create_task(_stats_loop(state, 10.0))
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
    p.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Log each forwarded datagram at DEBUG (very noisy on video)",
    )
    p.add_argument(
        "--public-ip",
        metavar="ADDR",
        default=None,
        help="Elastic IP / DNS of **this** relay host; logs explicit Quest vs robot UDP targets (recommended)",
    )
    args = p.parse_args()
    if args.verbose:
        logging.getLogger().setLevel(logging.DEBUG)
    try:
        asyncio.run(
            run_relay(args.bind, args.from_leader, args.from_follower, args.public_ip)
        )
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

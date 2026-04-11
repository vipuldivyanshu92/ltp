#!/usr/bin/env python3
"""
Send a few UDP datagrams to the relay follower leg (default port 5001).

Use this to verify the VPS security group / host firewall accepts inbound UDP on the
follower port **from whatever network you run this script on**. On the relay VM you
should then see `follower leg: learned UDP endpoint <this machine's public IP>:...`
and non-zero `F` counts in stats.

This does **not** prove the Quest app reads `adb shell setprop ...`; it only proves
the relay path is reachable from your current network.

**Do not run this from the same PC/network as `ltp-robot-gateway`.** The relay will
remember your NAT (ip,port) as the "follower" and send Quest video to that socket
instead of the headset. Use phone LTE, another VPS, or a colleague's machine; or
restart the relay after testing.
"""

from __future__ import annotations

import argparse
import socket
import time


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("host", help="Relay public IP (e.g. 54.153.43.113)")
    p.add_argument(
        "port",
        type=int,
        nargs="?",
        default=5001,
        help="Follower leg port (default 5001)",
    )
    p.add_argument("-n", type=int, default=5, help="Number of datagrams")
    args = p.parse_args()

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 0))
    local = s.getsockname()
    print(f"bound local UDP {local[0]}:{local[1]} -> {args.host}:{args.port}")
    for i in range(args.n):
        msg = f"RELAY_SMOKE_{i}".encode()
        s.sendto(msg, (args.host, args.port))
        print(f"sent {len(msg)} bytes")
        time.sleep(0.3)
    s.close()
    print("done — check relay logs for follower leg learned + F>0")


if __name__ == "__main__":
    main()

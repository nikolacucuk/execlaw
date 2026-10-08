"""Reject non-loopback IP connection and datagram attempts in denied-network tests."""

from __future__ import annotations

import argparse
import ipaddress
import re
from pathlib import Path

_SYSCALL_RE = re.compile(r"\b(?:connect|sendto|sendmsg)\(")
_ADDRESS_RE = re.compile(
    r'inet_addr\("(?P<v4>[^"]+)"\)|'
    r'inet_pton\(AF_INET6,\s*"(?P<v6>[^"]+)"\)'
)


def unexpected_addresses(trace: str) -> list[tuple[int, str]]:
    """Return attempted non-loopback IP destinations and their trace line numbers."""
    unexpected: list[tuple[int, str]] = []
    for line_number, line in enumerate(trace.splitlines(), start=1):
        if not _SYSCALL_RE.search(line):
            continue
        for match in _ADDRESS_RE.finditer(line):
            address = match.group("v4") or match.group("v6")
            try:
                parsed = ipaddress.ip_address(address)
            except ValueError:
                unexpected.append((line_number, address))
                continue
            if not parsed.is_loopback:
                unexpected.append((line_number, address))
    return unexpected


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("trace", type=Path)
    args = parser.parse_args()
    unexpected = unexpected_addresses(args.trace.read_text(encoding="utf-8", errors="replace"))
    if unexpected:
        for line_number, address in unexpected[:100]:
            print(f"line {line_number}: external network attempt to {address}")
        print(f"failed: {len(unexpected)} non-loopback network attempts")
        return 1
    print("passed: no non-loopback connect/sendto/sendmsg attempts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

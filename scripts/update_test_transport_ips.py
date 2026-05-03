#!/usr/bin/env python3
"""Rewrite Docker test transport IPs from a single per-lab mapping.

This keeps the lab compose files on distinct Docker bridge subnets so test
stacks do not collide when stale networks still exist on the host.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent

LAB_NETWORKS = {
    "single-hop.yml": {
        "subnet": "172.30.0.0/24",
        "ips": {
            "gateway": "172.30.0.10",
            "client": "172.30.0.30",
        },
    },
    "multi-hop-relay.yml": {
        "subnet": "172.31.0.0/24",
        "ips": {
            "gateway": "172.31.0.10",
            "relay": "172.31.0.20",
            "client": "172.31.0.30",
        },
    },
    "multi-hop-routing.yml": {
        "subnet": "172.32.0.0/24",
        "ips": {
            "gateway": "172.32.0.10",
            "relay1": "172.32.0.20",
            "relay2": "172.32.0.21",
            "client": "172.32.0.30",
        },
    },
    "peer-discovery.yml": {
        "subnet": "172.33.0.0/24",
        "ips": {
            "gateway": "172.33.0.10",
            "relay": "172.33.0.20",
            "client": "172.33.0.30",
        },
    },
    "resilience.yml": {
        "subnet": "172.34.0.0/24",
        "ips": {
            "gateway": "172.34.0.10",
            "client": "172.34.0.30",
        },
    },
    "flow-control.yml": {
        "subnet": "172.35.0.0/24",
        "ips": {
            "gateway": "172.35.0.10",
            "flood-sender": "172.35.0.30",
        },
    },
    "multi-gateway.yml": {
        "subnet": "172.36.0.0/24",
        "ips": {
            "gateway1": "172.36.0.10",
            "gateway2": "172.36.0.11",
            "relay": "172.36.0.20",
            "client": "172.36.0.30",
        },
    },
}


def rewrite_compose(path: Path, subnet: str, service_ips: dict[str, str]) -> bool:
    original = path.read_text()
    lines = original.splitlines()

    current_service: str | None = None
    in_services = False
    changed = False

    for index, line in enumerate(lines):
        stripped = line.strip()

        if stripped == "services:":
            in_services = True
            current_service = None
            continue

        if not line.startswith(" "):
            current_service = None
            if stripped == "networks:":
                in_services = False

        if in_services and line.startswith("  ") and stripped.endswith(":") and not line.startswith("    "):
            current_service = stripped[:-1]
            continue

        if current_service and "ipv4_address:" in stripped:
            expected_ip = service_ips.get(current_service)
            if expected_ip is None:
                raise ValueError(f"{path}: no mapped IP for service {current_service!r}")
            updated = re.sub(r"ipv4_address:\s+\S+", f"ipv4_address: {expected_ip}", line)
            if updated != line:
                lines[index] = updated
                changed = True
            continue

        if "subnet:" in stripped:
            updated = re.sub(r"subnet:\s+\S+", f"subnet: {subnet}", line)
            if updated != line:
                lines[index] = updated
                changed = True

    updated_text = "\n".join(lines) + "\n"
    if updated_text != original:
        path.write_text(updated_text)
        changed = True

    return changed


def rewrite_resilience_test(path: Path, resilience_gateway_ip: str) -> bool:
    original = path.read_text()
    updated = re.sub(
        r'RESILIENCE_GATEWAY_IP="[^"]+"',
        f'RESILIENCE_GATEWAY_IP="{resilience_gateway_ip}"',
        original,
    )
    if updated != original:
        path.write_text(updated)
        return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if files are out of date",
    )
    args = parser.parse_args()

    changed_paths: list[Path] = []

    for filename, config in LAB_NETWORKS.items():
        path = REPO_ROOT / "docker" / "compose" / filename
        changed = rewrite_compose(path, config["subnet"], config["ips"])
        if changed:
            changed_paths.append(path)

    resilience_test_path = REPO_ROOT / "docker" / "tests" / "test-resilience.sh"
    changed = rewrite_resilience_test(
        resilience_test_path,
        LAB_NETWORKS["resilience.yml"]["ips"]["gateway"],
    )
    if changed:
        changed_paths.append(resilience_test_path)

    if args.check and changed_paths:
        for path in changed_paths:
            print(f"out of date: {path.relative_to(REPO_ROOT)}", file=sys.stderr)
        return 1

    for path in changed_paths:
        print(f"updated {path.relative_to(REPO_ROOT)}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

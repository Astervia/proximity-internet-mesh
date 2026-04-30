#!/usr/bin/env python3
"""
pim-bt-rfcomm-linux.py — Linux BT auto-discovery daemon over RFCOMM/SPP.

Symmetric counterpart to `pim-bt-rfcomm-mac` (Swift). Auto-detects paired
peers whose name starts with "PIM-", opens RFCOMM channel 22 (SPP), runs the
PIM handshake, and emits newline-delimited JSON events on stdout.

Both inbound (we are RFCOMM acceptor) and outbound (we initiate) are
supported. Each side can be the initiator; identity is exchanged via
Hello/HelloAck — see PROTOCOL.md.

Run (as root, because RFCOMM bind needs CAP_NET_BIND_SERVICE for ch < 31):

    sudo sdptool add --channel=22 SP                         # one-time per boot
    sudo hciconfig hci0 piscan                              # discoverable + scannable
    sudo python3 pim-bt-rfcomm-linux.py --name=PIM-gateway  # foreground
"""

import argparse
import json
import os
import secrets
import socket
import struct
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone

CHANNEL = 22
HELLO_VERSION = 1
MAX_FRAME = 65_536
DEFAULT_PREFIX = "PIM-"
DEFAULT_POLL = 30.0  # seconds
DEFAULT_PLATFORM = "linux"
DEFAULT_CAPS = ["mesh-v1"]


# ---------- newline-delimited JSON output ----------

_emit_lock = threading.Lock()


def emit(event: dict) -> None:
    with _emit_lock:
        sys.stdout.write(json.dumps(event, sort_keys=True) + "\n")
        sys.stdout.flush()


def log_err(msg: str) -> None:
    sys.stderr.write(f"[pim-bt-rfcomm-linux] {msg}\n")
    sys.stderr.flush()


# ---------- frame codec (u32 BE length prefix + utf-8 JSON payload) ----------


def encode_frame(obj: dict) -> bytes:
    payload = json.dumps(obj, sort_keys=True).encode("utf-8")
    if len(payload) > MAX_FRAME:
        raise ValueError("frame too large")
    return struct.pack(">I", len(payload)) + payload


class FrameReader:
    def __init__(self) -> None:
        self.buf = bytearray()

    def feed(self, chunk: bytes):
        self.buf.extend(chunk)
        out = []
        while True:
            if len(self.buf) < 4:
                break
            (n,) = struct.unpack(">I", bytes(self.buf[0:4]))
            if n == 0 or n > MAX_FRAME:
                raise ValueError(f"bad frame length {n}")
            if len(self.buf) < 4 + n:
                break
            payload = bytes(self.buf[4 : 4 + n])
            del self.buf[: 4 + n]
            out.append(payload)
        return out


# ---------- session ----------


class Session:
    """One RFCOMM channel ↔ one peer."""

    def __init__(self, sock: socket.socket, addr: str, args, initiator: bool) -> None:
        self.sock = sock
        self.bd_addr = addr
        self.args = args
        self.initiator = initiator
        self.reader = FrameReader()
        self.peer_info: dict = {}
        self.opened_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
        self.alive = True

    def send(self, obj: dict) -> None:
        try:
            self.sock.sendall(encode_frame(obj))
        except OSError as e:
            log_err(f"send to {self.bd_addr} failed: {e}")
            self.alive = False

    def hello(self) -> dict:
        return {
            "type": "hello",
            "v": HELLO_VERSION,
            "node_id": self.args.node_id,
            "name": self.args.name,
            "platform": DEFAULT_PLATFORM,
            "caps": DEFAULT_CAPS + (["gateway-v1"] if self.args.gateway else []),
        }

    def hello_ack(self) -> dict:
        d = self.hello()
        d["type"] = "hello-ack"
        return d

    def run(self) -> None:
        if self.initiator:
            self.send(self.hello())

        try:
            while self.alive:
                chunk = self.sock.recv(4096)
                if not chunk:
                    break
                try:
                    payloads = self.reader.feed(chunk)
                except ValueError as e:
                    log_err(f"decode error from {self.bd_addr}: {e}")
                    break
                for p in payloads:
                    self._handle(p)
        except OSError as e:
            log_err(f"recv from {self.bd_addr} ended: {e}")
        finally:
            try:
                self.sock.close()
            except OSError:
                pass
            emit({
                "event": "lost",
                "peer": self.peer_info or {"bd_addr": self.bd_addr},
                "reason": "channel_closed",
            })
            registry.remove(self.bd_addr)

    def _handle(self, payload: bytes) -> None:
        try:
            msg = json.loads(payload.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            log_err(f"bad payload from {self.bd_addr}")
            return
        t = msg.get("type")
        if t == "hello":
            self.peer_info = self._extract_identity(msg)
            self.send(self.hello_ack())
            self._emit_discovered()
        elif t == "hello-ack":
            self.peer_info = self._extract_identity(msg)
            self._emit_discovered()
        elif t == "error":
            emit({"event": "peer_error", "bd_addr": self.bd_addr, "detail": msg})
        else:
            emit({"event": "frame", "bd_addr": self.bd_addr, "type": t})

    @staticmethod
    def _extract_identity(msg: dict) -> dict:
        """Strip protocol meta fields (type, v); keep only peer identity."""
        return {k: v for k, v in msg.items() if k not in ("type", "v")}

    def _emit_discovered(self) -> None:
        peer = dict(self.peer_info)
        peer["bd_addr"] = self.bd_addr
        peer["since"] = self.opened_at
        emit({"event": "discovered", "peer": peer})


# ---------- registry of active sessions ----------


class Registry:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._d: dict = {}

    def has(self, addr: str) -> bool:
        with self._lock:
            return addr in self._d

    def put(self, addr: str, session: Session) -> None:
        with self._lock:
            self._d[addr] = session

    def remove(self, addr: str) -> None:
        with self._lock:
            self._d.pop(addr, None)


registry = Registry()


# ---------- inbound listener ----------


def acceptor_loop(args) -> None:
    s = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("00:00:00:00:00:00", args.channel))
    s.listen(8)
    emit({
        "event": "listening",
        "channel": args.channel,
        "name": args.name,
        "node_id": args.node_id,
        "prefix": args.prefix,
    })
    while True:
        try:
            client, addr = s.accept()
        except OSError as e:
            log_err(f"accept failed: {e}")
            time.sleep(1)
            continue
        bd_addr = addr[0] if isinstance(addr, tuple) else str(addr)
        emit({"event": "inbound", "bd_addr": bd_addr})
        if registry.has(bd_addr):
            client.close()
            continue
        sess = Session(client, bd_addr, args, initiator=False)
        registry.put(bd_addr, sess)
        threading.Thread(target=sess.run, daemon=True, name=f"sess-{bd_addr}").start()


# ---------- outbound discovery (scan paired BT devices) ----------


def list_paired_pim_devices(prefix: str):
    """Return list of (addr, name) for PIM-prefixed paired devices via bluetoothctl."""
    try:
        out = subprocess.run(
            ["bluetoothctl", "devices", "Paired"],
            capture_output=True, text=True, timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        log_err(f"bluetoothctl error: {e}")
        return []

    devices = []
    for line in (out.stdout or "").splitlines():
        # `Device AA:BB:CC:DD:EE:FF Name` form (BlueZ ≥ 5.62 with `Paired` filter).
        parts = line.strip().split(maxsplit=2)
        if len(parts) >= 3 and parts[0] == "Device":
            addr = parts[1]
            name = parts[2]
            if name.startswith(prefix):
                devices.append((addr, name))
    return devices


def outbound_loop(args) -> None:
    while True:
        for addr, name in list_paired_pim_devices(args.prefix):
            if registry.has(addr):
                continue
            emit({"event": "scan_attempt", "bd_addr": addr, "name": name,
                  "channel": args.channel})
            sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
            sock.settimeout(15)
            try:
                sock.connect((addr, args.channel))
                sock.settimeout(None)
            except (OSError, socket.timeout) as e:
                emit({"event": "open_failed", "bd_addr": addr, "name": name,
                      "reason": str(e)})
                sock.close()
                continue
            sess = Session(sock, addr, args, initiator=True)
            registry.put(addr, sess)
            threading.Thread(target=sess.run, daemon=True, name=f"out-{addr}").start()
        time.sleep(args.poll)


# ---------- main ----------


def parse_args():
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", default="PIM-linux")
    ap.add_argument("--node-id", default=None,
                    help="32-byte hex; random if omitted")
    ap.add_argument("--prefix", default=DEFAULT_PREFIX)
    ap.add_argument("--channel", type=int, default=CHANNEL)
    ap.add_argument("--poll", type=float, default=DEFAULT_POLL)
    ap.add_argument("--no-outbound", action="store_true",
                    help="disable outbound discovery (acceptor-only mode)")
    ap.add_argument("--gateway", action="store_true",
                    help="advertise gateway-v1 capability")
    ns = ap.parse_args()
    if ns.node_id is None:
        ns.node_id = secrets.token_hex(32)
    return ns


def main() -> int:
    args = parse_args()
    emit({
        "event": "boot",
        "name": args.name,
        "node_id": args.node_id,
        "prefix": args.prefix,
        "channel": args.channel,
        "poll_s": args.poll,
        "outbound_enabled": not args.no_outbound,
        "gateway": args.gateway,
    })

    threading.Thread(target=acceptor_loop, args=(args,), daemon=True,
                     name="acceptor").start()
    if not args.no_outbound:
        threading.Thread(target=outbound_loop, args=(args,), daemon=True,
                         name="outbound").start()

    try:
        while True:
            time.sleep(60)
    except KeyboardInterrupt:
        emit({"event": "shutdown", "reason": "sigint"})
        return 0


if __name__ == "__main__":
    sys.exit(main())

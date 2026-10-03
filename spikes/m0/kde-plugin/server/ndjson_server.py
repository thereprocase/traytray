#!/usr/bin/env python3
"""M0.2 spike: NDJSON test server on a Unix socket.

Sends one frame per second to each connected client and logs every frame it
receives. A fixed schedule also sends boundary-size and oversized lines so the
client's 256 KB line limit can be checked from the client's own counters.
Standard library only.
"""

import argparse
import json
import os
import socket
import sys
import threading
import time

MAX_FRAME = 256 * 1024
SOCKET_NAME = "traytray-spike-m0.sock"


def log(msg):
    print(f"[{time.time():.3f}] {msg}", flush=True)


def padded_frame(seq, total_len):
    """Returns a JSON object line (without newline) of exactly total_len bytes."""
    head = f'{{"type":"pad","seq":{seq},"pad":"'
    tail = '"}'
    pad = total_len - len(head) - len(tail)
    if pad < 0:
        raise ValueError("total_len too small")
    return (head + "x" * pad + tail).encode()


# seq -> (description, line bytes). Each special frame replaces that second's heartbeat.
def schedule():
    return {
        4: ("boundary: exactly MAX_FRAME bytes, expect ACCEPT", padded_frame(4, MAX_FRAME)),
        6: ("boundary: MAX_FRAME + 1 bytes, expect REJECT", padded_frame(6, MAX_FRAME + 1)),
        8: ("oversized: 4 MiB, expect REJECT", padded_frame(8, 4 * 1024 * 1024)),
        10: ("not JSON, expect REJECT", b"this is not json"),
    }


def serve_client(conn, duration, stop):
    special = schedule()

    def reader():
        buf = b""
        while not stop.is_set():
            try:
                data = conn.recv(65536)
            except OSError:
                return
            if not data:
                log("client closed")
                return
            buf += data
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                log(f"RECV {len(line)}B {line[:200].decode(errors='replace')}")

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    seq = 0
    start = time.monotonic()
    try:
        while not stop.is_set() and time.monotonic() - start < duration:
            seq += 1
            if seq in special:
                desc, line = special[seq]
                log(f"SEND seq={seq} {len(line)}B ({desc})")
            else:
                line = json.dumps(
                    {"type": "heartbeat", "seq": seq, "text": f"frame {seq} from test server"},
                    separators=(",", ":"),
                ).encode()
                log(f"SEND seq={seq} {len(line)}B {line.decode()}")
            conn.sendall(line + b"\n")
            time.sleep(1)
    except OSError as e:
        log(f"send failed: {e}")
    finally:
        conn.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--duration", type=float, default=30.0, help="seconds per client")
    ap.add_argument("--lifetime", type=float, default=90.0, help="seconds before the server exits")
    args = ap.parse_args()

    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        sys.exit("XDG_RUNTIME_DIR is not set")
    path = os.path.join(runtime, SOCKET_NAME)
    if os.path.exists(path):
        sys.exit(f"{SOCKET_NAME} already exists; refusing to replace it")

    # Create the socket node with mode 0600, matching the design's local transport.
    old = os.umask(0o177)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    os.umask(old)
    srv.listen(4)
    srv.settimeout(1.0)
    log(f"listening on $XDG_RUNTIME_DIR/{SOCKET_NAME} mode {oct(os.stat(path).st_mode & 0o777)}")

    stop = threading.Event()
    deadline = time.monotonic() + args.lifetime
    try:
        while time.monotonic() < deadline:
            try:
                conn, _ = srv.accept()
            except socket.timeout:
                continue
            log("client connected")
            threading.Thread(
                target=serve_client, args=(conn, args.duration, stop), daemon=True
            ).start()
    finally:
        stop.set()
        srv.close()
        os.unlink(path)
        log("socket removed, exiting")


if __name__ == "__main__":
    main()

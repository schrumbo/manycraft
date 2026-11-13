#!/usr/bin/env python3
"""Fake Manycraft manager for exercising the companion mod by hand.

Run:  python tools/fake_manager.py [--port 34567] [--preview PATH] [--control FILE]
Then: MANYCRAFT_TOKEN=dev ./gradlew runClient -Pmanycraft.ipc=127.0.0.1:34567 \
        -Pmanycraft.instance=dev -Pmanycraft.preview=PATH -Pmanycraft.token=dev
Commands (stdin, or lines appended to --control FILE):
  show X Y W H | hide | profile FPS RD INTERVAL_MS WIDTH | chat TEXT | reconnect
  hotkeys NEXT PREV NEXT_ATTENTION GRID | quit | raw JSON
"""
import argparse
import json
import os
import socket
import struct
import sys
import threading
import time

MAGIC = 0x4D435056
conn = None
lock = threading.Lock()
preview_path = None


def log(*parts):
    print(time.strftime("%H:%M:%S"), *parts, flush=True)


def check_preview(msg):
    if not preview_path:
        return
    try:
        with open(preview_path, "rb") as f:
            head = f.read(16)
            magic, seq, w, h = struct.unpack("<IIII", head)
            f.seek(16)
            pixels = f.read(w * h * 4) if w and h else b""
        ok = magic == MAGIC and seq % 2 == 0 and seq == msg["seq"] and len(pixels) == w * h * 4
        nonblack = sum(1 for i in range(0, len(pixels), 4 * 97) if pixels[i:i + 3] != b"\0\0\0")
        log(f"  preview file: magic={'ok' if magic == MAGIC else hex(magic)} seq={seq} {w}x{h} "
            f"even={seq % 2 == 0} match_msg={seq == msg['seq']} nonblack_samples={nonblack} -> {'OK' if ok else 'BAD'}")
    except Exception as e:
        log("  preview file check failed:", e)


def send(obj):
    with lock:
        if conn is None:
            log("no client connected")
            return
        conn.sendall((json.dumps(obj) + "\n").encode())
    log("->", json.dumps(obj))


def command(line):
    parts = line.strip().split(None, 1)
    if not parts:
        return
    name = parts[0]
    rest = parts[1] if len(parts) > 1 else ""
    args = rest.split()
    if name == "show":
        x, y, w, h = map(int, args[:4])
        send({"t": "show", "x": x, "y": y, "w": w, "h": h})
    elif name == "hide":
        send({"t": "hide"})
    elif name == "profile":
        keys = ["fps", "render_distance", "preview_interval_ms", "preview_width"]
        send({"t": "profile", **{k: int(v) for k, v in zip(keys, args)}})
    elif name == "chat":
        send({"t": "chat_send", "text": rest})
    elif name == "reconnect":
        send({"t": "reconnect"})
    elif name == "quit":
        send({"t": "quit"})
    elif name == "hotkeys":
        keys = ["next", "prev", "next_attention", "grid"]
        send({"t": "hotkeys", **dict(zip(keys, args))})
    elif name == "raw":
        send(json.loads(rest))
    else:
        log("unknown command", name)


def stdin_loop():
    for line in sys.stdin:
        command(line)


def control_loop(path):
    open(path, "a").close()
    with open(path) as f:
        f.seek(0, os.SEEK_END)
        while True:
            line = f.readline()
            if line:
                command(line)
            else:
                time.sleep(0.2)


def serve(port):
    global conn
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(1)
    log(f"listening on 127.0.0.1:{port}")
    while True:
        c, addr = srv.accept()
        log("client connected", addr)
        with lock:
            conn = c
        try:
            with c.makefile("r", encoding="utf-8") as r:
                for line in r:
                    try:
                        msg = json.loads(line)
                    except ValueError:
                        log("bad json:", line.strip())
                        continue
                    shown = dict(msg)
                    if shown.get("t") == "hello":
                        shown["token"] = "<%d chars>" % len(shown.get("token", ""))
                    log("<-", json.dumps(shown))
                    if msg.get("t") == "preview":
                        check_preview(msg)
        except OSError:
            pass
        log("client disconnected")
        with lock:
            conn = None


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=34567)
    ap.add_argument("--preview")
    ap.add_argument("--control")
    a = ap.parse_args()
    preview_path = a.preview
    if a.control:
        threading.Thread(target=control_loop, args=(a.control,), daemon=True).start()
    threading.Thread(target=stdin_loop, daemon=True).start()
    serve(a.port)

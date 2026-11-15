"""Local offline-mode Minecraft server for testing Manycraft with offline accounts.

testserver start [--version 26.3] [--port 25565]   download if needed and start in the background
testserver cmd <console command>                     run a command over RCON, e.g. "kick Dev Test", "tell Dev hi"
testserver status                                    running or not, and the player list
testserver stop                                      stop the server
"""
import argparse
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(tempfile.gettempdir()) / "manycraft-testserver"
RCON_PORT = 25575
RCON_PASSWORD = "manycraft-test"
MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"


def fetch_json(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return json.load(r)


def java():
    found = shutil.which("java")
    if not found:
        sys.exit("java not found on PATH (needs the Java version the server requires)")
    return found


def install(version):
    folder = ROOT / version
    jar = folder / "server.jar"
    if not jar.exists():
        folder.mkdir(parents=True, exist_ok=True)
        entry = next(v for v in fetch_json(MANIFEST)["versions"] if v["id"] == version)
        url = fetch_json(entry["url"])["downloads"]["server"]["url"]
        print(f"downloading server {version}")
        urllib.request.urlretrieve(url, jar)
    return folder


def write_config(folder, port):
    (folder / "eula.txt").write_text("eula=true\n")
    props = {
        "online-mode": "false",
        "server-ip": "127.0.0.1",
        "server-port": str(port),
        "enable-rcon": "true",
        "rcon.port": str(RCON_PORT),
        "rcon.password": RCON_PASSWORD,
        "spawn-protection": "0",
        "max-players": "500",
        "motd": "Manycraft test server",
        "view-distance": "4",
        "simulation-distance": "4",
        "level-type": "minecraft\\:flat",
        "generate-structures": "false",
        "spawn-monsters": "false",
        "white-list": "false",
    }
    path = folder / "server.properties"
    if not path.exists():
        props["difficulty"] = "peaceful"
    else:
        kept = [l.partition("=")[2].strip() for l in path.read_text().splitlines() if l.startswith("difficulty=")]
        if kept:
            props["difficulty"] = kept[0]
    path.write_text("".join(f"{k}={v}\n" for k, v in props.items()))


def rcon(command):
    with socket.create_connection(("127.0.0.1", RCON_PORT), timeout=10) as s:
        def send(kind, body, rid):
            data = struct.pack("<ii", rid, kind) + body.encode() + b"\x00\x00"
            s.sendall(struct.pack("<i", len(data)) + data)

        def recv():
            size = struct.unpack("<i", s.recv(4))[0]
            data = b""
            while len(data) < size:
                data += s.recv(size - len(data))
            rid, _ = struct.unpack("<ii", data[:8])
            return rid, data[8:-2].decode(errors="replace")

        send(3, RCON_PASSWORD, 1)
        if recv()[0] == -1:
            raise RuntimeError("rcon login refused")
        send(2, command, 2)
        return recv()[1]


def running():
    try:
        rcon("list")
        return True
    except OSError:
        return False


def start(version, port):
    if running():
        print("already running")
        return 0
    folder = install(version)
    write_config(folder, port)
    log = open(folder / "console.log", "ab")
    flags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS if os.name == "nt" else 0
    subprocess.Popen([java(), "-Xmx1G", "-jar", "server.jar", "nogui"], cwd=folder, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=flags, start_new_session=os.name != "nt")
    for _ in range(180):
        if running():
            print(f"server {version} ready on 127.0.0.1:{port}, log {folder / 'console.log'}")
            return 0
        time.sleep(1)
    print(f"server did not come up, see {folder / 'console.log'}")
    return 1


def main():
    parser = argparse.ArgumentParser(prog="testserver", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("start")
    s.add_argument("--version", default="26.3")
    s.add_argument("--port", type=int, default=25565)
    c = sub.add_parser("cmd")
    c.add_argument("command", nargs="+")
    sub.add_parser("status")
    sub.add_parser("stop")
    args = parser.parse_args()
    if args.cmd == "start":
        return start(args.version, args.port)
    if not running():
        print("not running")
        return 0 if args.cmd in ("status", "stop") else 1
    if args.cmd == "cmd":
        print(rcon(" ".join(args.command)))
    elif args.cmd == "status":
        print("running: " + rcon("list"))
    else:
        rcon("stop")
        print("stopping")
    return 0


if __name__ == "__main__":
    sys.exit(main())

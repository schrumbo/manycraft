"""Local offline-mode Minecraft server for testing Manycraft with offline accounts.

testserver start [--version 26.3] [--port 25565] [--hooks]   download if needed and start in the background
testserver [--port 25565] cmd <console command>                run a command over RCON, e.g. "kick Dev Test", "tell Dev hi"
testserver [--port 25565] status                               running or not, and the player list
testserver [--port 25565] stop                                 stop the server

A port other than 25565 gets its own folder and RCON on port + 10, so it runs beside the shared server.
--hooks puts testhooks/ ahead of the server jar: /testopen <player> <pos> opens a container for a player and
/testmap <player> <digits> gives a locked map with digits drawn on it, which vanilla cannot do without the player.
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
import zipfile
from pathlib import Path

ROOT = Path(tempfile.gettempdir()) / "manycraft-testserver"
DEFAULT_PORT = 25565
HOOKS = Path(__file__).resolve().parent / "testhooks"
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


def rcon_port(port):
    return port + 10


def folder_of(version, port):
    return ROOT / (version if port == DEFAULT_PORT else f"{version}-{port}")


def install(version, port):
    folder = folder_of(version, port)
    jar = folder / "server.jar"
    shared = folder_of(version, DEFAULT_PORT) / "server.jar"
    if not jar.exists() and shared.exists() and shared != jar:
        folder.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(shared, jar)
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
        "rcon.port": str(rcon_port(port)),
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
        "allow-flight": "true",
    }
    if os.environ.get("MANYCRAFT_TESTSERVER_CONDUCT"):
        props["enable-code-of-conduct"] = "true"
    path = folder / "server.properties"
    if not path.exists():
        props["difficulty"] = "peaceful"
    else:
        kept = [l.partition("=")[2].strip() for l in path.read_text().splitlines() if l.startswith("difficulty=")]
        if kept:
            props["difficulty"] = kept[0]
    path.write_text("".join(f"{k}={v}\n" for k, v in props.items()))


def rcon(command, port=DEFAULT_PORT):
    with socket.create_connection(("127.0.0.1", rcon_port(port)), timeout=10) as s:
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


def running(port=DEFAULT_PORT):
    try:
        rcon("list", port)
        return True
    except OSError:
        return False


def hooked(folder):
    if not (folder / "versions").exists():
        subprocess.run([java(), "-DbundlerMainClass=net.minecraft.data.Main", "-jar", "server.jar", "--help"], cwd=folder, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    out = folder / "hooks"
    out.mkdir(exist_ok=True)
    game = next((folder / "versions").glob("*/*.jar"))
    unsigned = out / game.name
    if not unsigned.exists() or unsigned.stat().st_mtime < game.stat().st_mtime:
        with zipfile.ZipFile(game) as src, zipfile.ZipFile(unsigned, "w", zipfile.ZIP_DEFLATED) as dst:
            for item in src.infolist():
                if not (item.filename.startswith("META-INF/") and item.filename.rsplit(".", 1)[-1] in ("SF", "RSA", "EC", "DSA")):
                    dst.writestr(item, src.read(item))
    jars = [unsigned, *(folder / "libraries").rglob("*.jar")]
    classpath = os.pathsep.join(str(j) for j in jars)
    subprocess.run(["javac", "-nowarn", "-cp", classpath, "-d", str(out), *[str(s) for s in HOOKS.glob("*.java")]], check=True)
    return ["-cp", os.pathsep.join([str(out), classpath]), "net.minecraft.server.Main"]


def start(version, port, hooks):
    if running(port):
        print("already running")
        return 0
    folder = install(version, port)
    write_config(folder, port)
    log = open(folder / "console.log", "ab")
    flags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS if os.name == "nt" else 0
    launch = hooked(folder) if hooks else ["-jar", "server.jar"]
    server = subprocess.Popen([java(), "-Xmx1G", *launch, "nogui"], cwd=folder, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=flags, start_new_session=os.name != "nt")
    for _ in range(180):
        if running(port):
            print(f"server {version} ready on 127.0.0.1:{port}, log {folder / 'console.log'}")
            return 0
        if server.poll() is not None:
            break
        time.sleep(1)
    print(f"server did not come up, see {folder / 'logs' / 'latest.log'}")
    return 1


def main():
    parser = argparse.ArgumentParser(prog="testserver", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    sub = parser.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("start")
    s.add_argument("--version", default="26.3")
    s.add_argument("--port", type=int, default=argparse.SUPPRESS)
    s.add_argument("--hooks", action="store_true")
    c = sub.add_parser("cmd")
    c.add_argument("command", nargs="+")
    sub.add_parser("status")
    sub.add_parser("stop")
    args = parser.parse_args()
    if args.cmd == "start":
        return start(args.version, args.port, args.hooks)
    if not running(args.port):
        print("not running")
        return 0 if args.cmd in ("status", "stop") else 1
    if args.cmd == "cmd":
        print(rcon(" ".join(args.command), args.port))
    elif args.cmd == "status":
        print("running: " + rcon("list", args.port))
    else:
        rcon("stop", args.port)
        print("stopping")
    return 0


if __name__ == "__main__":
    sys.exit(main())

import hashlib
import json
import os
import shutil
import sys
import uuid

home, count = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 3
shutil.rmtree(home, ignore_errors=True)
os.makedirs(home)
if count <= 0:
    sys.exit(0)


def offline_uuid(name):
    digest = bytearray(hashlib.md5(f"OfflinePlayer:{name}".encode()).digest())
    digest[6] = digest[6] & 0x0F | 0x30
    digest[8] = digest[8] & 0x3F | 0x80
    return uuid.UUID(bytes=bytes(digest))


def write(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(value, f, indent=2)


accounts = [{"id": str(offline_uuid(f"Dev{n:03}")), "name": f"Dev{n:03}", "kind": "offline"} for n in range(1, count + 1)]
write(os.path.join(home, "accounts.json"), accounts)
names = ["Test", "Farm", "Mine", "Lobby", "Event"]
groups = max(1, min(int(os.environ.get("APP_BATCHES", "1")), len(names), count))
for g in range(groups):
    batch_id = "fixture" + str(g) * 25
    write(os.path.join(home, "batches", batch_id, "batch.json"), {
        "id": batch_id,
        "name": names[g],
        "game_version": "26.3",
        "loader_version": None,
        "mods": [],
        "server": None,
        "memory_mb": 2048,
        "accounts": [a["id"] for i, a in enumerate(accounts) if i % groups == g],
        "jvm_args": [],
    })

"""Regenerates src/core/headless/v777.rs from a Minecraft server jar: packet ids from the data generator's packets.json,
the protocol number from version.json, the English texts the connection-only client shows for chat, deaths and kicks,
and from the game code itself (GameData.java) the map colours, items, menu types, data component types and how each
block state looks in a picture; the default durability of items from the generator's item component reports.

headless-packets [server.jar]   default: the test server's 26.3 jar
"""
import json
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "src" / "core" / "headless" / "v777.rs"
GAME_DATA = Path(__file__).resolve().parent / "GameData.java"
DEFAULT_JAR = Path(tempfile.gettempdir()) / "manycraft-testserver" / "26.3" / "server.jar"
LANG_PREFIXES = ("container.", "chat.type.", "commands.message.display.", "death.", "multiplayer.disconnect.", "multiplayer.player.", "multiplayer.requiredTexturePrompt.", "disconnect.", "connect.failed", "deathScreen.title")

PACKETS = {
    "login": {
        "clientbound": ["login_disconnect", "hello", "login_finished", "login_compression", "custom_query", "cookie_request"],
        "serverbound": ["hello", "key", "custom_query_answer", "login_acknowledged", "cookie_response"],
    },
    "configuration": {
        "clientbound": ["cookie_request", "disconnect", "finish_configuration", "keep_alive", "ping", "registry_data", "resource_pack_pop", "resource_pack_push", "transfer", "select_known_packs", "code_of_conduct"],
        "serverbound": ["client_information", "cookie_response", "custom_payload", "finish_configuration", "keep_alive", "pong", "resource_pack", "select_known_packs", "accept_code_of_conduct"],
    },
    "play": {
        "clientbound": ["chunk_batch_finished", "cookie_request", "disconnect", "disguised_chat", "keep_alive", "ping", "player_chat", "player_combat_kill", "player_position", "resource_pack_pop", "resource_pack_push", "respawn", "set_health", "start_configuration", "system_chat", "transfer", "open_screen", "container_set_content", "container_set_slot", "container_close", "set_cursor_item", "set_player_inventory", "set_held_slot", "map_item_data", "set_title_text", "set_subtitle_text", "set_action_bar_text", "clear_titles", "login", "level_chunk_with_light", "forget_level_chunk", "block_update", "section_blocks_update", "light_update", "add_entity", "remove_entities", "move_entity_pos", "move_entity_pos_rot", "move_entity_rot", "rotate_head", "teleport_entity", "entity_position_sync", "set_entity_data", "set_equipment", "player_info_update", "player_info_remove", "set_time", "game_event", "set_experience", "update_attributes", "update_mob_effect", "remove_mob_effect", "set_objective", "set_display_objective", "set_score", "reset_score", "set_player_team", "boss_event", "set_titles_animation"],
        "serverbound": ["accept_teleportation", "chat_ack", "chat_command_signed", "chat", "chunk_batch_received", "client_command", "configuration_acknowledged", "cookie_response", "keep_alive", "player_loaded", "pong", "resource_pack", "container_click", "container_close"],
    },
}


def rust_str(s):
    return json.dumps(s, ensure_ascii=False)


def game_data(work, inner):
    libraries = [str(p) for p in (Path(work) / "libraries").rglob("*.jar")]
    out = Path(work) / "gamedata.tsv"
    classpath = __import__("os").pathsep.join([str(inner), *libraries])
    subprocess.run(["java", "-cp", classpath, str(GAME_DATA), str(out), str(inner)], cwd=work, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    rows = {"color": [], "item": [], "menu": [], "component": [], "consume_effect": [], "block": [], "entity": [], "villager_type": [], "villager_profession": [], "attribute": [], "mob_effect": [], "particle": [], "serializer": [], "accessor": []}
    for line in out.read_text(encoding="utf-8").splitlines():
        kind, *fields = line.split("\t")
        rows[kind].append(fields)
    for kind in rows:
        if kind == "accessor":
            continue
        rows[kind].sort(key=lambda r: int(r[0]))
        assert [int(r[0]) for r in rows[kind]] == list(range(len(rows[kind]))), f"{kind} ids are not dense"
    return rows


def const_name(name):
    return name.upper().replace("/", "_")


def snake(name):
    out = ""
    for i, c in enumerate(name.replace("$", "_")):
        if c.isupper() and i and out[-1] != "_":
            out += "_"
        out += c.lower()
    return out


def main():
    jar = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_JAR
    with tempfile.TemporaryDirectory() as work:
        subprocess.run(["java", "-DbundlerMainClass=net.minecraft.data.Main", "-jar", str(jar), "--reports", "--output", str(Path(work) / "out")], cwd=work, check=True, stdout=subprocess.DEVNULL)
        packets = json.loads((Path(work) / "out" / "reports" / "packets.json").read_text(encoding="utf-8"))
        item_report = {p.stem: json.loads(p.read_text(encoding="utf-8")) for p in (Path(work) / "out" / "reports" / "minecraft" / "components" / "item").glob("*.json")}
        inner = next((Path(work) / "versions").glob("*/*.jar"))
        with zipfile.ZipFile(inner) as z:
            version = json.loads(z.read("version.json"))
            lang = json.loads(z.read("assets/minecraft/lang/en_us.json"))
        data = game_data(work, inner)
    lines = [
        f"//! Generated by `.claude/bin/headless-packets` from the Minecraft {version['id']} server jar. Do not edit.",
        "",
        f"/// The protocol number of Minecraft {version['id']}.",
        f"pub const PROTOCOL: i32 = {version['protocol_version']};",
        "",
    ]
    for state, sides in PACKETS.items():
        lines.append(f"pub mod {state} {{")
        for side, names in sides.items():
            lines.append(f"    pub mod {side} {{")
            for name in names:
                pid = packets[state][side][f"minecraft:{name}"]["protocol_id"]
                lines.append(f"        pub const {name.upper()}: i32 = {pid};")
            lines.append("    }")
        lines.append("}")
        lines.append("")
    texts = sorted((k, v) for k, v in lang.items() if k.startswith(LANG_PREFIXES))
    lines.append("/// English texts by translation key, sorted by key.")
    lines.append("pub static TEXTS: &[(&str, &str)] = &[")
    for k, v in texts:
        lines.append(f"    ({rust_str(k)}, {rust_str(v)}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Map colours by packed id (base colour times four plus shade) as RGBA; ids without a colour are transparent.")
    lines.append("pub static MAP_COLORS: [[u8; 4]; 256] = [")
    for _, argb in data["color"]:
        v = int(argb) & 0xFFFFFFFF
        a, r, g, b = v >> 24, (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF
        if a == 0:
            r = g = b = 0
        lines.append(f"    [{r}, {g}, {b}, {a}],")
    lines.append("];")
    lines.append("")
    lines.append("/// Items by protocol id: name, English name, and the map colour of its block as `0xRRGGBB` (`None` for items that are no block).")
    lines.append("pub static ITEMS: &[(&str, &str, Option<u32>)] = &[")
    for _, name, key, color in data["item"]:
        shown = lang.get(key, name.replace("_", " ").title())
        rgb = f"Some(0x{int(color):06X})" if int(color) > 0 else "None"
        lines.append(f"    ({rust_str(name)}, {rust_str(shown)}, {rgb}),")
    lines.append("];")
    lines.append("")
    lines.append("/// How many block states the game has.")
    lines.append(f"pub const BLOCK_STATES: usize = {len(data['block'])};")
    lines.append("")
    lines.append("/// How block states look in a picture, as runs from the first state of each: 0 for air, else the map colour in the low six")
    lines.append("/// bits and 1 for a fluid or 2 for a plant above them.")
    lines.append("pub static BLOCK_LOOKS: &[(u16, u8)] = &[")
    previous = None
    for state, color, kind in data["block"]:
        look = 0 if kind == "0" else ({"2": 1, "4": 2}.get(kind, 0) << 6) | int(color)
        if look != previous:
            lines.append(f"    ({state}, {look}),")
            previous = look
    lines.append("];")
    lines.append("")
    lines.append("/// Menu types by protocol id.")
    lines.append("pub static MENUS: &[&str] = &[")
    for _, name in data["menu"]:
        lines.append(f"    {rust_str(name)},")
    lines.append("];")
    lines.append("")
    lines.append("/// Data component types by protocol id.")
    lines.append("pub mod component {")
    for pid, name in data["component"]:
        lines.append(f"    pub const {const_name(name)}: i32 = {pid};")
    lines.append("}")
    lines.append("")
    lines.append("/// Consume effect types by protocol id.")
    lines.append("pub mod consume_effect {")
    for pid, name in data["consume_effect"]:
        lines.append(f"    pub const {const_name(name)}: i32 = {pid};")
    lines.append("}")
    lines.append("")
    lines.append("/// Entity types by protocol id.")
    lines.append("pub static ENTITY_TYPES: &[&str] = &[")
    for _, name in data["entity"]:
        lines.append(f"    {rust_str(name)},")
    lines.append("];")
    lines.append("")
    for kind, title in (("villager_type", "Villager types"), ("villager_profession", "Villager professions"), ("attribute", "Attributes"), ("mob_effect", "Mob effects")):
        lines.append(f"/// {title} by protocol id.")
        lines.append(f"pub static {kind.upper()}S: &[&str] = &[")
        for _, name in data[kind]:
            lines.append(f"    {rust_str(name)},")
        lines.append("];")
        lines.append("")
    lines.append("/// Particle types by protocol id, and whether the type carries no options.")
    lines.append("pub static PARTICLE_TYPES: &[(&str, bool)] = &[")
    for _, name, simple in data["particle"]:
        lines.append(f"    ({rust_str(name)}, {'true' if simple == '1' else 'false'}),")
    lines.append("];")
    lines.append("")
    lines.append("/// How much damage items that break survive by default, by item protocol id, sorted.")
    lines.append("pub static MAX_DAMAGE: &[(u16, u16)] = &[")
    ids = {name: int(pid) for pid, name, *_ in data["item"]}
    durable = sorted((ids[name], entry.get("components", {}).get("minecraft:max_damage", 0)) for name, entry in item_report.items())
    for item, most in durable:
        if most > 0:
            lines.append(f"    ({item}, {most}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Entity data serializers by protocol id.")
    lines.append("pub mod serializer {")
    for pid, name in data["serializer"]:
        lines.append(f"    pub const {name}: i32 = {pid};")
    lines.append("}")
    lines.append("")
    lines.append("/// Entity data indices by entity class, as the game assigns them.")
    lines.append("#[allow(dead_code)]")
    lines.append("pub mod entity_data {")
    by_class = {}
    for cls, field, index, _ in data["accessor"]:
        by_class.setdefault(snake(cls), []).append((field, index))
    for cls in sorted(by_class):
        lines.append(f"    pub mod {cls} {{")
        for field, index in sorted(by_class[cls], key=lambda f: int(f[1])):
            lines.append(f"        pub const {field}: u8 = {index};")
        lines.append("    }")
    lines.append("}")
    OUT.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)}: protocol {version['protocol_version']}, {len(texts)} texts, {len(data['item'])} items, {len(data['component'])} components")


if __name__ == "__main__":
    main()
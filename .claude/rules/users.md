# Users

Provisional archetypes, from the owner's own use and server rule pages (caps of 1–10 alts per player are common; bots are what servers ban). Revise when real user feedback arrives.

| | Archetype | Test with |
|---|---|---|
| A | Alt holder: 2–6 accounts on one survival or factions server to keep farms loaded, Windows, 16 GB, mouse, never edited JVM args | mouse only, zero setup, everyday German, five instances fit next to a browser |
| B | AFK farmer: 10–50 accounts on one server, plays one while the rest idle, checks in every hour | status of every account at a glance, swap in one click or key, hidden instances cost nothing, a server restart reconnects all 50 without a hand |
| C | Operator: several batches with different mods and configs, proxies per account, hundreds of accounts | keyboard and palette, multi-select and bulk actions, nothing scales worse than linearly, 200 tiles inside the frame budget |

A is the default user, B is the reason the app exists, C sets the ceiling. Nothing may require being C to use the basics; nothing may cap what C can do.

- Adding an account and starting a batch works with zero configuration. Settings refine; they never unlock the core.
- Proxies, JVM args, memory caps, join commands and reconnect policy appear on first use or opt-in.
- Every command has a mouse path and a palette entry plus shortcut.
- Default paths use everyday German. No "JVM", "Heap", "SOCKS5" on a default path unless the user opened that setting.
- Attention is shown, never shouted: an account that needs work is highlighted in its tile and counted; one coalesced OS notification per incident, no modal alerts, no sound by default.
- Every drop shows its reason. Reconnect backs off with jitter, stops on a ban, and never retries a dead login.
- Manycraft is a manager, not a bot: it never generates movement, input or chat on its own. The only text it sends is the user's fixed join commands, once per join.
- Server rules are the user's business: the app ships no server list, never claims a server allows alts, and offers nothing that dodges a server's limits.
- Account tokens never leave the OS keychain in plain text; instance folders the user did not ask to change stay untouched.
- Windows, macOS and Linux count the same.

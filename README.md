<h1 align="center">Manycraft</h1>

<p align="center">One window for all your Minecraft instances.</p>

Manycraft is a desktop launcher built with Rust and [GPUI](https://www.gpui.rs) for running many Minecraft accounts at once, for example on servers that allow AFK alts. Instead of a window per account you get one grid of live previews; click a tile and that instance takes the window's place as a normal, fully playable game.

> **Status:** early development. Nothing is released yet. The first milestone (launcher core) is in progress.

## Planned for 1.0

- **Grid:** live previews of every instance, refreshed every few seconds
- **Swap:** a click or an in-game hotkey brings any instance into the window, at full speed and with your normal settings
- **Batches:** groups of instances with identical Fabric mods and configs; run several batches with different setups side by side
- **Accounts:** Microsoft accounts with a proxy per account, used for login and for the game connection
- **Attention:** accounts that dropped off their server are highlighted and counted, after auto-reconnect has tried
- **Light on resources:** hidden instances run at a minimal background profile, shared game files exist once on disk
- **Chat:** one feed across all instances, private messages highlighted, commands sent to a whole batch; all optional

Manycraft runs on Windows, macOS and Linux and supports Fabric.

## Repository

| Path | What |
|---|---|
| `src/` | The app: `core/` (accounts, install, launch, networking), `ui/` (GPUI interface) |
| `mod/` | The Fabric companion mod that every instance runs (previews, background profile, status, proxy) |

## Build from source

Requires a current stable [Rust](https://rustup.rs) toolchain and, for the mod, JDK 25.

```sh
git clone https://github.com/schrumbo/manycraft.git
cd manycraft
cargo run
```

Build the companion mod with `./gradlew build` inside `mod/`.

On Debian or Ubuntu, install the system libraries first:

```sh
sudo apt-get install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libdbus-1-dev pkg-config
```

Run the tests with `cargo test`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright (c) 2026 Schrumbo. Licensed under the [GNU Affero General Public License v3.0](LICENSE).

Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.

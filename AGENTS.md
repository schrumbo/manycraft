# Manycraft

Rust/GPUI desktop app that runs many Minecraft instances behind one window, plus a Fabric companion mod in `mod/`. Losing accounts, tokens or instance data is the worst possible bug. Feel target: Figma; design language: Apple's Human Interface Guidelines on every OS. Built for the archetypes in `.claude/rules/users.md`.

## Commands

- Check everything: `.claude/bin/gate` (clippy and UI lint on changed lines, tests, and the mod or agent build when `mod/` or `agent/` changed)
- Tests: `cargo nextest run`, or `cargo test`
- Companion agent: `./gradlew build` in `agent/` (frozen mod: same in `mod/`)
- Developer CLI: `cargo run -- login`, `cargo run -- launch --version 26.3`
- Delete a test home: `.claude/bin/rmtest C:/mc-<name>` (unlinks junctions first, never follows them)
- Local offline-mode server for tests with offline accounts: `.claude/bin/testserver start|status|cmd <command>|stop` (RCON, e.g. `cmd kick Dev Test`, `cmd tell Dev hi`). Test with offline accounts; there is no Microsoft account to test with.
- Every test client starts muted: `soundCategory_master:0.0` in the batch options template (`batches/<id>/options.txt` under `MANYCRAFT_HOME`) before launch. The key is shared across a batch, so a game dir `options.txt` alone does not survive the next start.

## Rules

- Instances stay real Minecraft windows. The manager shows previews and swaps the real window into its place; it never streams gameplay.
- Long-term win over quick win: fix root causes, keep modules deep and coherent, refactor what you touch when that is the clean path. No hacks, workarounds, duplicated logic or patched symptoms to finish faster. Maintainability and code quality outrank speed.
- Performance is a feature: hidden instances cost as little as possible, the manager idles at 0 frames. Budgets in `.claude/rules/performance.md`.
- Account tokens live only in the OS keychain and never appear in logs, errors or Debug output.
- A proxy set on an account applies to login and game connection; without the companion mod such an instance must not connect.
- UI work is verified by using the running app, not by reading code.
- Do not run `cargo fmt`; match the surrounding style.
- No code comments. Doc comments only on public items, in the existing terse style.
- Commit at stable points, stage files by name, no co-author lines. Push and release only at milestones; every push costs CI minutes.
- The owner works from a Mac and a Windows PC. Run `.claude/bin/sync` before work and before each commit; never stash, merge-pull or force. Details in `.claude/rules/workflow.md`.

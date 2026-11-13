# Manycraft

Rust/GPUI desktop app that runs many Minecraft instances behind one window, plus a Fabric companion mod in `mod/`. Losing accounts, tokens or instance data is the worst possible bug. Feel target: Figma; design language: Apple's Human Interface Guidelines on every OS. Built for the archetypes in `.claude/rules/users.md`.

## Commands

- Check everything: `.claude/bin/gate` (clippy and UI lint on changed lines, tests, and the mod build when `mod/` changed)
- Tests: `cargo nextest run`, or `cargo test`
- Mod: `./gradlew build` in `mod/`
- Developer CLI: `cargo run -- login`, `cargo run -- launch --version 26.3`

## Rules

- Instances stay real Minecraft windows. The manager shows previews and swaps the real window into its place; it never streams gameplay.
- Performance is a feature: hidden instances cost as little as possible, the manager idles at 0 frames. Budgets in `.claude/rules/performance.md`.
- Account tokens live only in the OS keychain and never appear in logs, errors or Debug output.
- A proxy set on an account applies to login and game connection; without the companion mod such an instance must not connect.
- UI work is verified by using the running app, not by reading code.
- Do not run `cargo fmt`; match the surrounding style.
- No code comments. Doc comments only on public items, in the existing terse style.
- Commit at stable points, stage files by name, no co-author lines. Push and release only at milestones; every push costs CI minutes.
- The owner works from a Mac and a Windows PC. Run `.claude/bin/sync` before work and before each commit; never stash, merge-pull or force. Details in `.claude/rules/workflow.md`.

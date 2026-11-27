---
name: deliver
description: Default path for every code change in Manycraft beyond a few lines, from spec to commit, with agents for implementation, review and real-app testing. Use for features, bug fixes, refactors, UI and companion-mod work, without being asked.
---

# Deliver

1. **Spec.** State the outcome, the acceptance check and every state of the workflow it touches: entry, hover, press, focus, loading, empty, error, undo, exit. Name the archetypes from `.claude/rules/users.md` the change touches; A is always touched by user-facing changes. Design it here; this is where quality is decided. For UI, use the `apple-design` skill and read `~/.claude/skills/app-polish/references/feel.md` and `flows.md`; shorten the workflow before polishing it. Ask only about removals and remakes.
2. **Locate.** Use LSP and `Explore`. Do not read whole large files.
3. **Build.** Delegate to `rust-implementer` when the spec is clear and the change is mechanical or isolated. Do it yourself when it needs judgement across modules or crosses into `mod/`. Bugs start with a failing test.
4. **Gate.** `.claude/bin/gate` until green. Add `--deep` when `Cargo.toml` changed.
5. **Review.** In parallel, in one message:
   - always `rust-reviewer`
   - if `src/ui` changed: `ui-auditor` and `ux-tester`
   - if rendering, the grid, previews, launching or the companion mod changed: `perf-tester`
6. **Fix.** Verify each finding before acting on it. Fix the real ones, run the gate again, then re-review only what changed substantially. At most two rounds; after that report what is open. Two failed attempts on one bug: `root-cause`.
7. **Commit.** At a stable point: `.claude/bin/sync`, stage by name, search the diff for personal data, terse message, no co-author. Push only when a milestone is complete.
8. **Report.** What works now, with evidence. Anything open. What only a person can judge, as short manual test steps. How many commits are unpushed.

Changes under `src/core` that touch accounts, tokens or instance folders: also run `cargo mutants --in-diff <(git diff HEAD) -f 'src/core/**' --test-tool nextest` and add tests for survivors.

---
name: herd
description: Runs independent features in parallel as separate Antigravity (`agy`) sessions in their own git worktrees, visible in the Herdr sidebar. The owner's default whenever work splits into long tasks touching different files. Not for reviews or small tasks; those use subagents.
---

# Herd

Each helper is a full `agy` session with its own context in a Herdr terminal (`hc`), so it costs more than a subagent. Use it whenever tasks run long and do not overlap; otherwise use normal subagents. All helpers share `sccache` and the machine-wide build lock.

```sh
.agents/bin/herd new <name> "<exact task and acceptance check>"
.agents/bin/herd new <name> --model pro "<architecture-heavy task>"
.agents/bin/herd new <name> --model xhigh "<deep root-cause task>"
.agents/bin/herd status
.agents/bin/herd land <name>
.agents/bin/herd rm <name>
```

Model tiers (Matt Pocock tiering — never burn the heaviest model on routine work):
- default (`flash` -> `gemini-3.8-flash-high`): standard implementation tasks
- `--model fast` (`gemini-3.8-flash-low`): mechanical/trivial tasks
- `--model pro` (`gemini-4-neon-high`): architecture-heavy changes
- `--model xhigh` (`gemini-4-neon-xhigh`): hard root-cause or high-stakes judgment
- Any explicit `agy models` ID (e.g. `claude-sonnet-5-5-high`, `gemini-4-neon-medium`) also works.

Worktree: `.claude/worktrees/<name>`, branch `herd/<name>`, started from the last commit. Uncommitted work on main is invisible to helpers, so commit before spawning. Helpers run with `--mode auto --dangerously-skip-permissions`, follow `ship`, commit only on their own branch and report back via `hc send`.

1. Split the work so files do not overlap. Declare shared modules (`mod.rs`, `Cargo.toml` deps, entry points) on main first so helpers only fill their own files. Give each helper the task, the file scope and the acceptance check.
2. Keep working. Wait with `hc wait --children any`, never sleep.
3. Steer with `hc send <name> "..."`. Read with `hc read --lines 100 <name>`. Approve helper prompts with `hc keys <name> enter` unless they violate hard rules (owner's screen, real accounts/keychain, pushing, private data, deleting user data).
4. On a report, read the actual diff: `git diff main...herd/<name>`. Do not infer success from the report.
5. Land with `.agents/bin/herd land <name>` on a clean main: it merges with a merge commit and removes the worktree. Run `.claude/bin/gate --base HEAD~1` afterwards.

---
name: herd
description: Runs independent features in parallel as separate Claude sessions in their own git worktrees, visible in the Herdr sidebar. The owner's default whenever work splits into long tasks touching different files. Not for reviews or small tasks; those use subagents.
---

# Herd

Each helper is a full session with its own context, so it costs far more than a subagent. Use it whenever tasks run long and do not overlap; otherwise use normal subagents. All helpers share sccache and the machine-wide build lock.

```sh
.claude/bin/herd new <name> "<exact task and acceptance check>"
.claude/bin/herd new <name> --model opus "<task>"
.claude/bin/herd new <name> --lane "src/core/x/*,Cargo.toml" "<task>"
.claude/bin/herd status
.claude/bin/herd land <name>
.claude/bin/herd rm <name>
```

Worktree: `.claude/worktrees/<name>`, branch `herd/<name>`, started from the last commit. Uncommitted work on main is invisible to helpers, so commit before spawning. Default model Sonnet. Helpers follow `deliver`, commit only on their own branch and report back as `<hc from=...>`.

1. Give each helper a `--lane`: the only paths it may change. `herd land` refuses a diff outside it (`HERD_LANE_OK=1` overrides after a look). Split the work so files do not overlap. Declare shared modules (`mod.rs`, `Cargo.toml` deps, entry points) on main first so helpers only fill their own files. Give each helper the task, the file scope and the acceptance check.
2. Keep working. Wait with `hc wait --children any`, never sleep.
3. Steer with `hc send <name> "..."`. Read with `hc read --lines 100 <name>`.
4. On a report, read the actual diff: `git diff main...herd/<name>`. Do not infer success from the report.
5. Land with `herd land <name>` on a clean main: it merges with a merge commit and removes the worktree. Run `.claude/bin/gate --base HEAD~1` afterwards.

---
trigger: always_on
---

# Workflow

Standing instruction from the owner: **this codebase is yours** (`feedback-codebase-is-yours.md`). Act as owner, not as a guest — fix what is broken, refactor, improve tooling, commit at stable points, and tag/push/release at milestones without asking permission. Product decisions, roadmap, Freya IA reset and current pause status live in `~/.claude/projects/<project-slug>/memory/MEMORY.md` (read relevant files before making product or architecture decisions). Only what needs the owner's hands or real accounts goes to the owner, collected in milestone reports.

- Any change beyond a few lines follows the `ship` skill. Questions, reading and one-line fixes do not.
- Commit autonomously at stable points: stage by name, terse message, no co-author. Run `.claude/bin/sync` before each commit.
- Push and release autonomously at milestones (end of M1, M2, …) by bumping `version` in `Cargo.toml` in the milestone's last commit, tagging/pushing, then watching CI and fixing any failures until green. Never push work in progress between milestones (saves GitHub CI minutes).
- A Stop hook runs `.claude/bin/gate` (clippy, tests, UI lint, and the mod or agent build when `mod/` or `agent/` changed) and asks for missing reviews. Treat its message as a task. If a finding is a false positive or a review does not apply, say why in one line and stop again.
- At most two review rounds per feature. Round two only checks round-one fixes; new findings go to the backlog, except data loss (accounts, tokens, worlds, configs).
- Set `CARGO_TERM_QUIET=true`, `CARGO_TERM_COLOR=never`, `CARGO_TERM_PROGRESS_WHEN=never`, `RUST_BACKTRACE=1` when running Cargo commands directly, and write files with LF line endings (`\n`, never CRLF).

## Model Tiering (Matt Pocock Setup)

Never use the strongest model where a lighter tier fits. Always pass the matching `Model` tier on `invoke_subagent` and `--model` on `.agents/bin/herd`:

| Work | Who | Subagent `Model` | Herdr `agy --model` |
|---|---|---|---|
| Decisions, architecture, integration | main session | default (`pro`) | — |
| Finding code across many files | `research` | `flash_lite` | — |
| UI checklist on a diff | `ui-auditor` | `flash_lite` | — |
| Implementing a clear, bounded spec | `rust-implementer` | `flash` | `flash` (`gemini-3.8-flash-high`, default) |
| Using the real app, screenshots, hover | `ux-tester` | `flash` | — |
| Frame times, idle cost, instance cost | `perf-tester` | `flash` | — |
| Correctness review of a diff | `rust-reviewer` | `pro` | `pro` (`gemini-4-neon-high`) |
| Bug still alive after two fix attempts | `root-cause` | `pro` | `xhigh` (`gemini-4-neon-xhigh`) |
| Taste, scope, product / UI approval (Fable role) | dedicated `pro` / `xhigh` agent | `pro` | `xhigh` (`gemini-4-neon-xhigh`) |

- Parallel work goes through the `herd` skill (`.agents/bin/herd`, Herdr helpers running `agy` in git worktrees) whenever tasks run long and touch different files; that is the owner's default. Only when it does not pay off, use normal subagents or `swarm` with `workspace: "share"`. Never parallel edits in one tree.
- Give each agent the exact scope and acceptance check, not the conversation.
- A UI change is not done until it was used in the running app, and performance not until it was measured.
- Design questions go through the `apple-design` skill with the archetypes from `.claude/rules/users.md` as the audience.
- Scripts before agents: anything `gate` can answer needs no model.

## Two machines

The owner works on a Mac (`ssh mac`) and a Windows PC (`ssh windows`), each with its own clone; origin is the only link; only milestone pushes reach it, so the other machine may lag until then. Session start runs `.claude/bin/sync`; act on its report before editing. Never stash, never merge-pull, never force. When stopping, say how many commits are unpushed.

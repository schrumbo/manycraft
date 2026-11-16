# Workflow

Standing instruction from the owner: use the agents below without being asked, and work autonomously up to 1.0. Taste, scope and product questions go to an agent on model `fable`, which answers in the owner's interest; only what needs the owner's hands or accounts goes to the owner, collected in milestone reports.

- Any change beyond a few lines follows the `ship` skill. Questions, reading and one-line fixes do not.
- Commit at stable points yourself: stage by name, terse message, no co-author. Run `.claude/bin/sync` before each commit.
- Push only at milestones (end of M1, M2, …) and release only then, by bumping `version` in `Cargo.toml` in the milestone's last commit. Every push costs GitHub CI minutes; never push work in progress.
- A Stop hook runs `.claude/bin/gate` (clippy, tests, UI lint, and the mod or agent build when `mod/` or `agent/` changed) and asks for missing reviews. Treat its message as a task. If a finding is a false positive or a review does not apply, say why in one line and stop again.
- At most two review rounds per feature. Round two only checks round-one fixes; new findings go to the backlog, except data loss (accounts, tokens, worlds, configs).

| Work | Who | Model |
|---|---|---|
| Decisions, architecture, integration | main session | Opus |
| Finding code across many files | `Explore` | small default |
| Implementing a clear, bounded spec | `rust-implementer` | Sonnet |
| Correctness review of a diff | `rust-reviewer` | Opus |
| UI checklist on a diff | `ui-auditor` | Haiku |
| Using the real app, screenshots, hover | `ux-tester` | Sonnet |
| Frame times, idle cost, instance cost | `perf-tester` | Sonnet |
| Bug still alive after two fix attempts | `root-cause` | Fable |

- Parallel work goes through the `herd` skill (Herdr helpers in worktrees) whenever tasks run long and touch different files; that is the owner's default. Only when it does not pay off, use normal subagents. Never parallel edits in one tree.
- Give each agent the exact scope and acceptance check, not the conversation.
- A UI change is not done until it was used in the running app, and performance not until it was measured.
- Visible UI changes land only after the owner said yes on the running build: screenshots before and after, one area per approval.
- Design questions go through the `apple-design` skill with the archetypes from `.claude/rules/users.md` as the audience.
- Scripts before agents: anything `gate` can answer needs no model.

## Two machines

The owner works on a Mac (`ssh mac`) and a Windows PC (`ssh windows`), each with its own clone; origin is the only link; only milestone pushes reach it, so the other machine may lag until then. Session start runs `.claude/bin/sync`; act on its report before editing. Never stash, never merge-pull, never force. When stopping, say how many commits are unpushed.

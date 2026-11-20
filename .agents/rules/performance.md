---
trigger: glob
globs: "**/src/**, **/mod/**, **/agent/**"
description: Performance constraints and budgets for Manycraft core and companion mod.
---

# Performance

Manycraft exists to run many instances on one machine. Every instance the user is not looking at costs as little as possible.

- A hidden instance runs at its background profile (low fps cap, minimal render distance, no particles, muted), applied by the companion mod, not by the user.
- A shown instance gets its normal settings back exactly.
- The manager idles at 0 frames and near 0 CPU when nothing changes; previews update only for tiles on screen.
- Preview frames travel through shared memory, downscaled in the instance, never encoded per frame.
- Shared game files (assets, libraries, Java runtime, a batch's mods) exist once on disk.
- Budgets for the perf-tester: manager frame p95 under 8.3 ms with 200 tiles; a hidden instance under 3 % of one core and inside its memory cap after warm-up.

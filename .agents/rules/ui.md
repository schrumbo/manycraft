---
trigger: glob
globs: "**/src/ui/**"
description: UI design language, interaction standards, and GPUI traps for src/ui.
---

# UI

The design language follows Apple's Human Interface Guidelines on every OS. Design and review every screen with the `apple-design` skill.

Every interactive element needs all of these, or a stated reason:

- Hover and press: `.soft(color, radius)` from `ui/motion.rs` on `Stateful<Div>`, before adding children.
- Keyboard: the action reachable without a mouse, with palette entry and shortcut.
- Pointer: `.cursor_pointer()`. Icon-only controls get a tooltip.
- States: disabled, loading, empty, error and selected are designed, not defaulted, in light and dark.
- One ring per tile, priority selection > hover > attention.

Values come from `ui/theme.rs` (spacing, radius, font, colors), motion from `ui/motion.rs`. New values go into a token, not inline. Text goes through `tr()`.

Feel: the next frame reacts. No blocking work on the UI thread: launching, downloads, auth and instance I/O run off it. Motion is interruptible, under 300 ms, and absent on frequent keyboard actions.

The standard is Figma. Before designing or changing an interaction, read `~/.claude/skills/app-polish/references/feel.md`; for workflows also `flows.md`, for input `inputs.md`, in the same folder.

## GPUI traps

- Elements cannot be scaled.
- `request_animation_frame` only in render; in a handler it crashes.
- Updating an entity from inside its own update panics. In subscriptions on input fields use App-level `cx.subscribe`, and move focus changes to the next frame.
- `group_drag_over` needs its own hitbox: add `.hover(|s| s)`.
- Inputs let Enter propagate, so an action and `PressEnter` can both fire.
- Instance previews are textures updated in place; never rebuild the grid per frame.

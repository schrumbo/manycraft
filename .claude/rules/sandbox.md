---
paths:
  - "src/core/sandbox/**"
  - "src/core/sandbox.rs"
---

# Sandbox

- Change rights on drive roots and on folders above the Manycraft home only with `SetFileSecurityW` and no inheritance. Never `icacls /grant` or `/remove` on `C:\` or `C:\Users`: it walks the whole disk for over an hour.
- Every elevated (UAC) step needs the owner's yes first, and its undo is verified with a read-back before reporting done.

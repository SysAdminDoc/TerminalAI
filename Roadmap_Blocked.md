# Blocked roadmap items

## R-06 · P1 — Session hibernation and rehydration

Blocked pending live validation that `claude --resume <id>` restores enough context for
transparent hibernation. The implementation requires a real Claude session and operator-visible
judgment about whether the resumed context is equivalent; no safe local inference can settle that
question.

## R-56 · P2 — A UI test and screenshot path that actually works

Blocked 2026-08-03 by the required Windows visual-isolation display, not by the application test
code. The signed virtual display driver was healthy but Windows attached no isolated screen: the
approved `visual-isolation.ps1 ensure` failed closed before and after `remove` plus a verified
driver reinstall. The embedded WebDriver provider then timed out at the first `element` command,
and a direct native EdgeDriver probe independently failed with `DevToolsActivePort file doesn't
exist` after launching the app on the verified private desktop. Resume only after `ensure` returns
the exact fourth virtual display; do not use a physical monitor or an interactive desktop.

Update 2026-08-03 (later session): `ensure` succeeds again — `\\.\DISPLAY5` 1920x1080 at (5360,0)
attached, and `launch` placed the release app on it with placement proof. The display blocker is
gone; the WebDriver/EdgeDriver failures above are unretested since and are now the open question.

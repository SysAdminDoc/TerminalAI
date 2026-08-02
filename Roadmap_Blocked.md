# Blocked roadmap items

## R-06 · P1 — Session hibernation and rehydration

Blocked pending live validation that `claude --resume <id>` restores enough context for
transparent hibernation. The implementation requires a real Claude session and operator-visible
judgment about whether the resumed context is equivalent; no safe local inference can settle that
question.

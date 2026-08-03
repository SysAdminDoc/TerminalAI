# TerminalAI Roadmap

Single task tracker for this repo. Newest phase at the top; completed items are removed.

## v0.6.0 — the project factory (operator request, 2026-08-03)

- [ ] Usability and comprehension pass — a new operator should understand the fleet screen without
      reading the README. Cover: first-run guidance beyond the empty-state card, plain-language
      labels for status/dwell/attention states, tooltips on every control, and a short in-app
      explainer of the row → focused-terminal model. Acceptance: someone unfamiliar with the tool
      can launch a session, read its state, and answer an attention request unaided
- [ ] Roadmap scanner — for every known project, detect and parse `ROADMAP.md`, surfacing
      open-item counts and staleness per project so "which projects still have work queued" is a
      glance, not a hunt
- [ ] Per-session prompt queue — queue one or more prompts against a session while it is busy;
      when the current run completes, the next queued prompt is sent automatically. Queue is
      visible and reorderable per row, a queued prompt can be edited or withdrawn before it fires,
      and completion detection uses the same hook/status signal as the fleet's Idle state — never a
      timer. If the run ends in an attention state (approval or question), the queue pauses rather
      than answering blind
- [ ] Automated roadmap work queue — stored prompt library plus a queue that runs a chosen prompt
      against all (or selected) projects whose roadmaps still have open items, one session per
      project, honoring the fleet's admission/memory budget. First two stored prompts: "research
      new roadmap items" and "drain the roadmap". Queue survives restarts, reports per-project
      outcomes, and never launches into a repo with uncommitted changes without flagging it
      (builds on v0.4.0 broadcast; distinct from it — broadcast targets running sessions, the
      queue creates them)
      — *seed the library from the operator's existing templates: `~/.claude/prompts/research-deep.txt`
      (research new roadmap items — the source of this repo's `RESEARCH.md`) and
      `~/.claude/prompts/roadmap-drain.txt` (drain the roadmap). Both are 6–11 KB of prose, so the
      store must hold multi-KB text and deliver it as a pty write, never as an argv argument or a
      shell-interpolated string — see the native-binary resolution decision in `CLAUDE.md` for why
      `cmd.exe` quoting is not an option.*

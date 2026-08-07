// Render a real fleet row, so assertions about the row can read markup rather
// than grep source.
//
// Before `renderRow` was extracted into `rowMarkup.js` it was unreachable from a
// test, so everything about the row was asserted by regexing `main.js` for a
// contiguous run of source text. That was always a proxy: it passes when the
// source happens to contain the right characters and fails when a line is
// wrapped, neither of which is a fact about what the operator sees. Now the
// renderer is importable, so the real markup is.
//
// Not a `.test.mjs` file, so `node --test tests/*.test.mjs` does not run it.

import { createRowRenderer } from "../src/rowMarkup.js";

/// Stand-ins for the helpers `main.js` binds. Each returns something
/// recognisable rather than something realistic — these tests are about the
/// shape of the markup, and a value that says what produced it makes a failure
/// readable.
const DEPS = {
  STATUS_META: {
    working: { glyph: "◒", label: "status-working", tone: "yellow" },
    "needs-you": { glyph: "!", label: "status-needs-you", tone: "peach" },
    exited: { glyph: "×", label: "status-exited", tone: "overlay0" },
    queued: { glyph: "⏳", label: "status-queued", tone: "overlay0" },
  },
  state: { focused: null, wideMode: true },
  answerCountdownLabel: (session) => (session.status === "needs-you" ? "answers itself in 12s" : ""),
  cost: (value) => (value == null ? "—" : `$${value}`),
  dwell: () => "3m",
  escapeHtml: (value) =>
    String(value ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;"),
  folderLabel: (cwd) => String(cwd).split(/[\\/]/).pop(),
  groupChip: () => "",
  isAttention: (session) => session.status === "needs-you",
  lastActivity: (session) => session.last_line ?? "last output",
  lifecycleDetail: () => "",
  lifecycleLabel: (session) => session.status,
  lifecycleTone: (_session, meta) => meta.tone,
  memory: (bytes) => (bytes == null ? "—" : `${bytes}B`),
  ports: (list) => (list ?? []).join(", "),
  queueGlyph: (session) => (session.queued_prompts ? `Q${session.queued_prompts}` : "·"),
  t: (key) => key,
  toolProgress: (progress) => (progress ? `${progress.completed}/${progress.total}` : "—"),
};

const SESSION = {
  id: "s0001",
  name: "session",
  agent: "claude",
  status: "working",
  cwd: "C:/repos/project",
  branch: "main",
  restarts: 0,
  ports: [42000],
  model: "opus",
  effort: "high",
  memory_bytes: 1024,
  cost_usd: 1.25,
};

/// One row's markup. `session` overrides the fixture; `state` overrides the
/// live-state stand-in (`wideMode`, `focused`).
export function renderFixtureRow(session = {}, state = {}) {
  const renderRow = createRowRenderer({ ...DEPS, state: { ...DEPS.state, ...state } });
  return renderRow({ ...SESSION, ...session });
}

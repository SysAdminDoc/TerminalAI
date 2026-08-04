import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");

const summary = main.slice(main.indexOf("function renderSummary"), main.indexOf("\n/// How often pinned panes"));
const diagnostics = main.slice(main.indexOf("function renderDiagnostics"), main.indexOf("\nfunction formatReason"));

test("fleet summary and state counts skip unchanged DOM writes", () => {
  assert.match(summary, /const summaryMarkup =/);
  assert.match(summary, /if \(summary\.innerHTML !== summaryMarkup\) summary\.innerHTML = summaryMarkup;/);
  assert.match(summary, /if \(stateStrip\.innerHTML !== stateMarkup\) stateStrip\.innerHTML = stateMarkup;/);
  assert.match(summary, /if \(fleetCount\.textContent !== fleetCountText\) fleetCount\.textContent = fleetCountText;/);
  assert.doesNotMatch(summary, /fleet-spend.*addEventListener/);
});

test("diagnostics update dwell and status in place while the timeline is stable", () => {
  assert.match(diagnostics, /const structure = JSON\.stringify\(\[session\.id, history\]\);/);
  assert.match(diagnostics, /if \(host\.dataset\.diagnosticsStructure === structure\)/);
  assert.match(diagnostics, /currentDetail\.textContent =/);
  // The tone comes from lifecycleTone, not straight from the status: a session
  // the supervisor gave up on must not be the same grey as one that finished.
  assert.match(diagnostics, /glyph\.className = "status-glyph tone-" \+ lifecycleTone\(session, meta\);/);
  assert.match(diagnostics, /host\.innerHTML =/);
});

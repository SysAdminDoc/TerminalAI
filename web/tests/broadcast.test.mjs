import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registrySource } from "./registrySource.mjs";
import test from "node:test";

import { defaultSelection, ineligibleReason, isEligible, summarize, targets } from "../src/broadcast.js";
import { appSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const main = appSource();
const panel = readFileSync(
  new URL("../src/broadcastPanel.js", import.meta.url),
  "utf8",
).replace(/\r\n/g, "\n");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const app = appRustSource();

const session = (overrides) => ({ id: "s0001", name: "shop", status: "working", ...overrides });

test("a session waiting for a permission decision is never a target", () => {
  // A permission prompt is a specific question with a small set of valid
  // answers, and prompt text answers something — just not what was meant.
  assert.equal(ineligibleReason(session({ status: "needs-approval" })), "broadcast-skip-approval");
  assert.equal(isEligible(session({ status: "needs-approval" })), false);
});

test("a session merely asking a question is a target", () => {
  // Refusing this one too would make broadcast useless for its clearest case.
  assert.equal(isEligible(session({ status: "awaiting-input" })), true);
});

test("a focused pane with unsubmitted input is never a broadcast target", () => {
  assert.equal(
    ineligibleReason(session({ queue_paused: "focused_and_edited" })),
    "broadcast-skip-focused-edited",
  );
  assert.equal(isEligible(session({ queue_paused: "focused_and_edited" })), false);
});

test("sessions with no process behind them are not targets", () => {
  for (const status of ["exited", "queued"]) {
    assert.equal(ineligibleReason(session({ status })), "broadcast-skip-not-running", status);
  }
  assert.equal(ineligibleReason(undefined), "broadcast-skip-not-running");
});

/** The closing brace of `broadcast_eligibility` and of its `impl` block. */
const BLOCK_END = ["", "    }", "}"].join("\n");

test("the eligibility rule matches the daemon's, status for status", () => {
  // A UI that offers a session the daemon will refuse teaches the operator to
  // ignore refusals. The Rust side refuses needs-approval and anything with no
  // pty; queued and exited are the statuses that carry no process.
  const registry = registrySource();
  // To the end of the match block rather than a byte count: a fixed window
  // silently stops covering the last arm the moment one is added, which is
  // exactly what a cross-language agreement test must not do.
  const start = registry.indexOf("fn broadcast_eligibility");
  const rule = registry.slice(start, registry.indexOf(BLOCK_END, start));
  assert.match(rule, /SessionStatus::NeedsApproval => Some\(BroadcastRefusal::NeedsApproval\)/);
  assert.match(rule, /budget_exhausted => Some\(BroadcastRefusal::BudgetExhausted\)/);
  assert.match(rule, /operator_edited[\s\S]*BroadcastRefusal::FocusedAndEdited/);
  assert.match(rule, /entry\.pty\.is_none\(\) => Some\(BroadcastRefusal::NotRunning\)/);
});

test("every reason a session is skipped has a string to render", () => {
  const reasons = [
    ineligibleReason(session({ status: "needs-approval" })),
    ineligibleReason(session({ queue_paused: "focused_and_edited" })),
    ineligibleReason(session({ status: "exited" })),
    ineligibleReason(session({ budget_exhausted: true })),
  ];
  for (const reason of reasons) {
    assert.ok(new RegExp(`^${reason} = `, "m").test(ftl), `${reason} has no string`);
  }
});

test("ineligible sessions are listed, not hidden", () => {
  // Hiding them makes the fleet look smaller than it is at the moment the
  // operator is deciding who to send to.
  const rows = targets([session({ id: "s1", status: "exited" }), session({ id: "s2" })]);
  assert.equal(rows.length, 2);
  // Eligible first, so the common case is at the top.
  assert.equal(rows[0].session.id, "s2");
  assert.equal(rows[1].reason, "broadcast-skip-not-running");
});

test("only eligible sessions start ticked", () => {
  // Pre-ticking one that cannot receive the prompt produces a refusal nobody
  // asked for.
  const selection = defaultSelection([
    session({ id: "s1" }),
    session({ id: "s2", status: "needs-approval" }),
    session({ id: "s3", status: "exited" }),
  ]);
  assert.deepEqual(selection, ["s1"]);
});

test("the summary reports delivered and refused separately", () => {
  // "Sent", when four of nine were skipped, is exactly the failure the
  // per-session protocol exists to prevent.
  const summary = summarize([{ refusal: null }, { refusal: { kind: "not_running" } }, { refusal: null }]);
  assert.deepEqual(summary, { delivered: 2, refused: 1, total: 3 });
  assert.deepEqual(summarize([]), { delivered: 0, refused: 0, total: 0 });
});

test("the selection is re-checked at send time, not trusted from open time", () => {
  // A session can enter a permission prompt while the operator is typing.
  const send = panel.slice(panel.indexOf("async function sendBroadcast"));
  const body = send;
  assert.match(body, /readBroadcastSelection\(\)\.filter\(\(id\) =>\s*isEligible\(/);
});

test("a partial refusal keeps the operator's live broadcast selection", () => {
  // The dialog is re-rendered after a refusal so current eligibility and
  // labels are fresh. That render must read the boxes the operator left
  // checked, not the selection captured when the dialog opened.
  const sync = panel.slice(panel.indexOf("function syncBroadcastSelection"));
  assert.match(sync.slice(0, 180), /state\.broadcastSelection = readBroadcastSelection\(\)/);
  assert.match(
    main,
    /\$\("broadcast-list"\)\.addEventListener\("change", \(\) => syncBroadcastSelection\(\)\)/,
  );
  const send = panel.slice(panel.indexOf("async function sendBroadcast"));
  const refusal = send.slice(send.indexOf("} else {"), send.indexOf("\n    } catch"));
  assert.match(refusal, /syncBroadcastSelection\(\);\s*renderBroadcast\(\)/);
});

test("both numbers are shown whenever anything was refused", () => {
  const send = panel.slice(panel.indexOf("async function sendBroadcast"));
  const body = send;
  assert.match(body, /const \{ delivered, refused, total \} = summarize\(results\)/);
  assert.match(body, /refused\s*\?\s*`\$\{t\("broadcast-sent"/);
});

test("an empty prompt is refused before anything is sent", () => {
  const send = panel.slice(panel.indexOf("async function sendBroadcast"));
  assert.match(send.slice(0, 400), /if \(!text\) \{/);
  assert.ok(/^broadcast-empty-prompt = /m.test(ftl));
});

test("broadcast uses the same bracketed paste framing as a single reply", () => {
  // Without it a multi-line prompt is submitted one line at a time, so the
  // agent acts on the first fragment.
  assert.match(app, /fn broadcast_prompt\(/);
  assert.match(app, /format!\("\\u\{1b\}\[200~\{text\}\\u\{1b\}\[201~\\r"\)/);
});

test("the dialog is reachable and its controls are wired", () => {
  assert.match(html, /id="broadcast-dialog"/);
  assert.match(html, /id="broadcast-toggle"/);
  assert.match(main, /\$\("broadcast-toggle"\)\.addEventListener\("click", \(\) => openBroadcast\(\)\)/);
  assert.match(main, /\$\("send-broadcast-button"\)\.addEventListener\("click", \(\) => void sendBroadcast\(\)\)/);
});

test("session names reaching the target list are escaped", () => {
  const body = panel.slice(
    panel.indexOf("function renderBroadcast"),
    panel.indexOf("function openBroadcast"),
  );
  assert.match(body, /escapeHtml\(session\.name\)/);
  assert.match(body, /escapeHtml\(session\.id\)/);
  assert.match(body, /escapeHtml\(t\(reason\)\)/);
});

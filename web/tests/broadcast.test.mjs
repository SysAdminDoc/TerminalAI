import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { defaultSelection, ineligibleReason, isEligible, summarize, targets } from "../src/broadcast.js";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const app = readFileSync(new URL("../../crates/terminalai-app/src/main.rs", import.meta.url), "utf8");

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

test("sessions with no process behind them are not targets", () => {
  for (const status of ["exited", "queued"]) {
    assert.equal(ineligibleReason(session({ status })), "broadcast-skip-not-running", status);
  }
  assert.equal(ineligibleReason(undefined), "broadcast-skip-not-running");
});

test("the eligibility rule matches the daemon's, status for status", () => {
  // A UI that offers a session the daemon will refuse teaches the operator to
  // ignore refusals. The Rust side refuses needs-approval and anything with no
  // pty; queued and exited are the statuses that carry no process.
  const registry = readFileSync(
    new URL("../../crates/terminalai-core/src/registry.rs", import.meta.url),
    "utf8",
  );
  const rule = registry.slice(registry.indexOf("fn broadcast_eligibility"));
  assert.match(rule.slice(0, 900), /SessionStatus::NeedsApproval => Some\(BroadcastRefusal::NeedsApproval\)/);
  assert.match(rule.slice(0, 900), /entry\.pty\.is_none\(\) => Some\(BroadcastRefusal::NotRunning\)/);
});

test("every reason a session is skipped has a string to render", () => {
  const reasons = [
    ineligibleReason(session({ status: "needs-approval" })),
    ineligibleReason(session({ status: "exited" })),
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
  const send = main.slice(main.indexOf("async function sendBroadcast"));
  const body = send.slice(0, send.indexOf("\nfunction createOutputChannel"));
  assert.match(body, /readBroadcastSelection\(\)\.filter\(\(id\) =>\s*isEligible\(/);
});

test("both numbers are shown whenever anything was refused", () => {
  const send = main.slice(main.indexOf("async function sendBroadcast"));
  const body = send.slice(0, send.indexOf("\nfunction createOutputChannel"));
  assert.match(body, /const \{ delivered, refused, total \} = summarize\(results\)/);
  assert.match(body, /refused\s*\?\s*`\$\{t\("broadcast-sent"/);
});

test("an empty prompt is refused before anything is sent", () => {
  const send = main.slice(main.indexOf("async function sendBroadcast"));
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
  const render = main.slice(main.indexOf("function renderBroadcast"));
  const body = render.slice(0, render.indexOf("\nfunction openBroadcast"));
  assert.match(body, /escapeHtml\(session\.name\)/);
  assert.match(body, /escapeHtml\(session\.id\)/);
  assert.match(body, /escapeHtml\(t\(reason\)\)/);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { JSDOM } from "jsdom";

import { pendingApprovals, renderApprovals, requestLine, waitingSince } from "../src/approvals.js";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const catalog = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

const escapeHtml = (value) =>
  String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
const translate = (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key);
const deps = { escape: escapeHtml, translate, dwell: () => "3m" };

const session = (id, status, since, approval = null) => ({
  id,
  name: id,
  status,
  status_since: since,
  pending_approval: approval,
});

test("the longest wait comes first", () => {
  // An inbox sorted newest-first buries the session that has been blocked for
  // twenty minutes under the one that asked a second ago, which is how a fleet
  // starves one session behind a stream of new prompts.
  const waiting = pendingApprovals([
    session("recent", "needs-approval", 3_000),
    session("oldest", "needs-approval", 1_000),
    session("middle", "needs-you", 2_000),
  ]);
  assert.deepEqual(
    waiting.map((entry) => entry.id),
    ["oldest", "middle", "recent"],
  );
});

test("only sessions actually waiting on a person are listed", () => {
  const waiting = pendingApprovals([
    session("busy", "working", 1),
    session("done", "idle", 2),
    session("limited", "rate-limited", 3),
    session("asking", "needs-approval", 4),
    session("input", "awaiting-input", 5),
    session("you", "needs-you", 6),
  ]);
  assert.deepEqual(
    waiting.map((entry) => entry.id).sort(),
    ["asking", "input", "you"],
    "a rate-limited session is waiting on a provider, not on the operator",
  );
});

test("an unknown wait sorts last rather than first", () => {
  // Absence of a timestamp is not evidence of a long wait, and putting it at
  // the top would displace a session that has genuinely been blocked.
  const waiting = pendingApprovals([
    { id: "nostamp", name: "nostamp", status: "needs-approval" },
    session("known", "needs-approval", 5_000),
  ]);
  assert.deepEqual(
    waiting.map((entry) => entry.id),
    ["known", "nostamp"],
  );
});

test("the wait is measured from when the prompt arrived, not the status change", () => {
  // A session can change status several times while one prompt stands.
  const stamped = session("s1", "needs-approval", 9_000, {
    tool: "Bash",
    summary: "ls",
    since: 1_000,
  });
  assert.equal(waitingSince(stamped), 1_000);
  assert.equal(waitingSince(session("s2", "needs-approval", 9_000)), 9_000);
});

test("a request with nothing to say is still listed", () => {
  // The row the fleet cannot describe is exactly the one a human has to go and
  // look at. Hiding it would hide the worst case.
  const waiting = pendingApprovals([session("silent", "needs-approval", 1)]);
  assert.equal(waiting.length, 1);
  assert.equal(requestLine(waiting[0], translate), "approvals-unknown-request");
  const markup = renderApprovals(waiting, deps);
  assert.match(markup, /approvals-unknown-request/);
});

test("the request line uses whichever halves exist", () => {
  const line = (approval) =>
    requestLine(session("s", "needs-approval", 1, approval), translate);
  assert.equal(line({ tool: "Bash", summary: "rm -rf build" }), "Bash — rm -rf build");
  assert.equal(line({ tool: "Bash" }), "Bash");
  assert.equal(line({ summary: "rm -rf build" }), "rm -rf build");
  assert.equal(line({ tool: "  ", summary: "  " }), "approvals-unknown-request");
});

test("an agent-supplied request cannot inject markup", () => {
  // The tool name and summary are whatever the agent sent.
  const hostile = '"><img src=x onerror="alert(1)">';
  const markup = renderApprovals(
    [session(hostile, "needs-approval", 1, { tool: hostile, summary: hostile })],
    deps,
  );
  const dom = new JSDOM(`<body>${markup}</body>`);
  assert.equal(dom.window.document.querySelectorAll("img").length, 0);
  assert.equal(
    dom.window.document.querySelector("[data-approval-send]").dataset.approvalSend,
    hostile,
  );
});

test("nothing in the inbox approves anything by itself", () => {
  // The criticism levelled at a competitor was auto-approving on the
  // operator's behalf. There is no approve-all, no bypass toggle, and no
  // invented universal "yes" — what an agent accepts is its own prompt's
  // vocabulary.
  const markup = renderApprovals([session("s1", "needs-approval", 1, { tool: "Bash" })], deps);
  assert.doesNotMatch(markup, /approve-all|bypass|data-approve-all/i);
  assert.match(markup, /data-approval-reply=/, "an answer box, not an approve button");

  const fn = main.slice(main.indexOf("async function sendApproval"));
  const body = fn.slice(0, fn.indexOf("\n/// Show the saved layouts"));
  assert.match(body, /if \(!answer\) return;/, "an empty box sends nothing");
  // The same write the fleet row's reply box uses: it is the same act.
  assert.match(body, /invoke\("write_session"/);
  assert.doesNotMatch(body, /"y"|"yes"|"1"/, "no answer is invented for the operator");
});

test("an answer being typed survives an unrelated session updating", () => {
  // The inbox follows the fleet, so any session changing re-renders it. Wiping
  // a half-typed answer because a different session moved would make the inbox
  // unusable exactly when the fleet is busy.
  const fn = main.slice(main.indexOf("function renderApprovalInbox"));
  const body = fn.slice(0, fn.indexOf("\n/// Send the operator's answer"));
  assert.match(body, /body\.dataset\.signature === signature/, "unchanged inbox is left alone");
  assert.match(body, /const typed = new Map\(/, "typed text is carried across a re-render");
});

test("the inbox is a view over the snapshot, not a second source", () => {
  // A separate poll could disagree with the rows behind it.
  assert.match(main, /const waiting = pendingApprovals\(state\.sessions\)/);
  assert.doesNotMatch(main, /invoke\("(list_)?approvals?"/, "no separate fetch");
  assert.match(main, /renderApprovalInbox\(\);/);
});

test("the approvals dialog exists and announces its count", () => {
  assert.match(html, /id="approvals-dialog"/);
  assert.match(html, /id="approvals-count"[^>]*role="status"[^>]*aria-live="polite"/);
  assert.match(html, /id="approvals-toggle"/);
  for (const key of ["approvals-title", "approvals-empty", "approvals-unknown-request"]) {
    assert.ok(catalog.includes(`${key} =`), `${key} is missing from the catalog`);
  }
});

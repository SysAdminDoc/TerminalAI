import assert from "node:assert/strict";
import test from "node:test";

import { reconcileKeyedRows } from "../src/fleetRows.js";

class FakeRow {
  constructor(id) {
    this.dataset = { id };
    this.hidden = false;
    this.replyInput = { value: "", selectionStart: 0, selectionEnd: 0 };
    this.focused = false;
    this.parentNode = null;
  }

  querySelector(selector) {
    return selector === "input[data-reply]" ? this.replyInput : null;
  }

  remove() {
    this.parentNode?.removeChild(this);
  }
}

class FakeContainer {
  constructor() {
    this.children = [];
  }

  get firstElementChild() {
    return this.children[0] ?? null;
  }

  insertBefore(row, reference) {
    this.removeChild(row);
    const index = reference ? this.children.indexOf(reference) : this.children.length;
    this.children.splice(index < 0 ? this.children.length : index, 0, row);
    row.parentNode = this;
  }

  removeChild(row) {
    const index = this.children.indexOf(row);
    if (index >= 0) this.children.splice(index, 1);
    row.parentNode = null;
  }
}

function reconcile(container, sessions) {
  reconcileKeyedRows(
    container,
    sessions,
    (session) => session.id,
    (session) => new FakeRow(session.id),
    (row, session) => {
      row.label = session.label;
      row.nextStatus = session.status;
    },
    (row) => row.focused || row.replyInput.value.length > 0,
  );
}

test("preserves a focused reply and caret across five seconds of keyed updates", () => {
  const container = new FakeContainer();
  reconcile(container, [
    { id: "s0001", label: "api", status: "needs-you" },
    { id: "s0002", label: "docs", status: "working" },
  ]);
  const replyRow = container.children.find((row) => row.dataset.id === "s0001");
  replyRow.replyInput.value = "keep this draft";
  replyRow.replyInput.selectionStart = 10;
  replyRow.replyInput.selectionEnd = 10;
  replyRow.focused = true;

  for (let second = 1; second <= 5; second += 1) {
    reconcile(container, [
      { id: "s0001", label: `api-${second}`, status: second % 2 ? "working" : "needs-you" },
      { id: "s0002", label: "docs", status: second % 2 ? "needs-you" : "working" },
    ]);
    assert.equal(container.children.find((row) => row.dataset.id === "s0001"), replyRow);
    assert.equal(replyRow.replyInput.value, "keep this draft");
    assert.equal(replyRow.replyInput.selectionStart, 10);
    assert.equal(replyRow.replyInput.selectionEnd, 10);
    assert.equal(replyRow.focused, true);
  }
});

test("keeps a drafted row hidden rather than removing it from a filtered view", () => {
  const container = new FakeContainer();
  reconcile(container, [{ id: "s0001", label: "api", status: "needs-you" }]);
  const row = container.children[0];
  row.replyInput.value = "draft";

  reconcile(container, []);

  assert.equal(container.children[0], row);
  assert.equal(row.hidden, true);
  assert.equal(row.replyInput.value, "draft");
});

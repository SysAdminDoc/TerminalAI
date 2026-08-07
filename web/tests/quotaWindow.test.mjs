import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderWindowShares, unattributed, windowShares } from "../src/quotaWindow.js";
import { appSource } from "./appSource.mjs";

const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const main = appSource();

const OPTIONS = {
  escape: (value) =>
    String(value ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;"),
  translate: (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key),
  cost: (usd) => `$${usd.toFixed(2)}`,
  hours: 5,
};

function admission(overrides = {}) {
  return {
    spend_window_usd: 10,
    spend_window_hours: 5,
    spend_window_by_session: [
      { id: "s0002", usd: 6 },
      { id: "s0001", usd: 4 },
    ],
    spend_window_unattributed_usd: 0,
    ...overrides,
  };
}

test("the window breakdown is a share of the window, not of a session's lifetime", () => {
  const shares = windowShares(admission(), [{ id: "s0001", name: "review" }]);
  assert.deepEqual(
    shares.map((share) => [share.id, share.usd, Math.round(share.percent)]),
    [
      ["s0002", 6, 60],
      ["s0001", 4, 40],
    ],
  );
  // A session that has since exited still consumed the window; it is named by
  // id rather than dropped for having no row left.
  assert.equal(shares[0].name, null);
  assert.equal(shares[1].name, "review");
});

test("a share of an unknown total is null, never zero", () => {
  // Reporting 0% would claim the session consumed none of the window, which is
  // the opposite of "we cannot say what fraction this is".
  const shares = windowShares(admission({ spend_window_usd: 0 }));
  assert.equal(shares.length, 2);
  for (const share of shares) assert.equal(share.percent, null);
  const markup = renderWindowShares(admission({ spend_window_usd: 0 }), [], OPTIONS);
  assert.doesNotMatch(markup, /0%/);
});

test("spend with no owner is shown rather than dropped", () => {
  // A ledger restored from a store written before it had a session dimension
  // has money and no sessions. Omitting it makes an incomplete breakdown read
  // as a complete account of the window.
  const source = admission({ spend_window_unattributed_usd: 3 });
  assert.equal(unattributed(source), 3);
  const markup = renderWindowShares(source, [], OPTIONS);
  assert.match(markup, /quota-window-unattributed/);
  assert.match(markup, /\$3\.00/);
});

test("a window nobody has spent in renders nothing at all", () => {
  // Not an empty table: a question that has not arisen yet is not a table with
  // no rows.
  const empty = admission({ spend_window_by_session: [], spend_window_unattributed_usd: 0 });
  assert.equal(renderWindowShares(empty, [], OPTIONS), "");
  assert.equal(renderWindowShares(undefined, [], OPTIONS), "");
});

test("the estimate is labelled as this tool's arithmetic, not the provider's", () => {
  // The one thing this must never do is present its own figures as the
  // provider's accounting.
  const markup = renderWindowShares(admission(), [], OPTIONS);
  assert.match(markup, /quota-window-estimate/);
  assert.match(
    ftl,
    /^quota-window-estimate = .*not the provider's accounting\./m,
    "the wording must say whose arithmetic this is",
  );
});

test("session names reaching the table are escaped", () => {
  const markup = renderWindowShares(
    admission({ spend_window_by_session: [{ id: "s0001", usd: 1 }] }),
    [{ id: "s0001", name: '<img src=x onerror="alert(1)">' }],
    OPTIONS,
  );
  assert.doesNotMatch(markup, /<img/);
  assert.match(markup, /&lt;img/);
});

test("every string this section uses exists in the catalog", () => {
  // The Rust side is the only one that rejects a duplicate message id, and the
  // JS loader silently takes the last definition, so a missing key is the
  // failure mode that reaches the operator as a bare key on screen.
  const used = [
    "quota-window-title",
    "quota-window-session",
    "quota-window-share",
    "quota-window-estimate",
    "quota-window-unattributed",
  ];
  for (const key of used) {
    assert.ok(new RegExp(`^${key} =`, "m").test(ftl), `${key} is missing from the catalog`);
  }
});

test("the rollup renders the breakdown rather than a second dialog", () => {
  // Deliberately inside the rollup: it is the same arithmetic asked a different
  // question, and a new dialog would be a new surface to audit for no gain.
  assert.match(main, /import \{ renderWindowShares \} from "\.\/quotaWindow\.js";/);
  assert.match(main, /renderWindowShares\(state\.admission, sessions, \{/);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const helpers = readFileSync(new URL("../src/rateLimit.js", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");

test("a rate-limited row sorts with the attention states, not the busy ones", () => {
  // A limited session renders identically to a working one at a glance, so
  // sorting it with `working` would bury it in a busy fleet.
  const order = main.slice(main.indexOf("const STATUS_ORDER"), main.indexOf("const STATUS_META"));
  const value = (key) => Number(order.match(new RegExp(`"?${key}"?:\\s*([0-9.]+)`))[1]);
  assert.ok(value("rate-limited") > value("working"));
  assert.ok(value("rate-limited") > value("thinking"));
  assert.ok(value("rate-limited") < value("needs-you"));
});

test("the fleet's live count excludes rate-limited sessions", () => {
  // The daemon drops them from its admission count so a queued session can take
  // the slot; a header that still counted them would contradict the queue.
  assert.match(
    main,
    /!\["exited", "queued", "rate-limited"\]\.includes\(session\.status\)/,
  );
});

test("the header reports how many are limited and when the soonest reopens", () => {
  assert.match(main, /countMessage\("count-rate-limited", limited\.length\)/);
  assert.match(main, /rateLimitTitle\(limited, t\)/);
  // Behaviour of the helper itself is covered in rateLimitLabels.test.mjs.
  assert.match(helpers, /Math\.min\(\.\.\.resets\)/);
});

test("the limited row and header labels come from one place", () => {
  // They were duplicated in main.js, where neither could be executed by a test —
  // only grepped. Both now resolve through the extracted module.
  assert.match(main, /import \{[^}]*rateLimitTitle, rateLimitedLabel \} from "\.\/rateLimit\.js";/);
  assert.match(main, /return rateLimitedLabel\(session, t\);/);
  assert.doesNotMatch(main, /function resetMillis/);
});

test("every rate-limit string used by the renderer exists in the catalog", () => {
  const used = [...`${main}${helpers}`.matchAll(/t\("(rate-limit-[a-z-]+)"/g)].map((m) => m[1]);
  const counted = [...main.matchAll(/countMessage\("(count-rate-limited)"/g)].map((m) => m[1]);
  assert.ok(used.length >= 3, `expected the renderer to use several, found ${used}`);
  for (const key of used) {
    assert.ok(
      new RegExp(`^${key} =`, "m").test(ftl),
      `${key} is used by the renderer but missing from terminalai.ftl`,
    );
  }
  for (const key of counted) {
    assert.ok(new RegExp(`^${key}-one =`, "m").test(ftl), `${key}-one missing`);
    assert.ok(new RegExp(`^${key}-other =`, "m").test(ftl), `${key}-other missing`);
  }
  assert.match(ftl, /^status-rate-limited = /m);
});

test("quota headroom is read through the extracted module, not open-coded", () => {
  assert.match(main, /import \{ quotaLabel, quotaUnreportedLabel, rateLimitTitle, rateLimitedLabel \} from "\.\/rateLimit\.js";/);
  assert.match(main, /quotaLabel\(state\.sessions, t\)/);
  // Behaviour of the helpers is covered in rateLimitLabels.test.mjs.
  assert.match(helpers, /export function worstQuota/);
  for (const key of ["fleet-quota", "fleet-quota-unreported", "quota-used", "quota-reset-unreported", "quota-unreported"]) {
    assert.ok(new RegExp(`^${key} =`, "m").test(ftl), `${key} missing from terminalai.ftl`);
  }
});

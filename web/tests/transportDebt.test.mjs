import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { optionalSystemTimeMs, systemTimeMs } from "../src/time.js";
import { appSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const main = appSource();
const app = appRustSource();

test("terminal output uses the dedicated channel and agent events stay off the webview", () => {
  assert.doesNotMatch(main, /case "output"/);
  assert.match(app, /RegistryEvent::AgentEvent \{ \.\. \}/);
  assert.match(app, /app\.emit\("terminalai:event", event\)/);
});

test("row costs delegate to the rollup formatter", () => {
  const cost = main.slice(main.indexOf("function cost"), main.indexOf("function reviewNumber"));
  assert.match(cost, /formatCost\(value\)/);
  assert.doesNotMatch(cost, /toFixed\(2\)/);
});

test("numeric timestamps accept seconds and milliseconds", () => {
  assert.equal(optionalSystemTimeMs(1_785_758_400), 1_785_758_400_000);
  assert.equal(optionalSystemTimeMs(1_785_758_400_000), 1_785_758_400_000);
  assert.equal(
    optionalSystemTimeMs({ secs_since_epoch: 1_785_758_400, nanos_since_epoch: 0 }),
    1_785_758_400_000,
  );
  assert.equal(optionalSystemTimeMs("soon"), null);
  assert.equal(systemTimeMs("soon") <= Date.now(), true);
});

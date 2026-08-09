import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { moduleSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const startup = moduleSource("startup.js");
const operationalPanels = moduleSource("operationalPanels.js");
const app = appRustSource();

test("the daemon log panel is present and receives bounded WebView batches", () => {
  assert.match(html, /id="logs-toggle"/);
  assert.match(html, /id="logs-host"[^>]*aria-label="Daemon logs"/);
  assert.match(startup, /listen\("terminalai:logs"/);
  assert.match(operationalPanels, /state\.logs = state\.logs\.slice\(-256\)/);
  assert.match(app, /const LOG_BATCH_INTERVAL: Duration = Duration::from_millis\(100\)/);
  assert.match(app, /VecDeque::<LogEntry>/);
  assert.match(app, /app\.emit\("terminalai:logs", batch\)/);
});

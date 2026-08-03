import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const app = readFileSync(new URL("../../crates/terminalai-app/src/main.rs", import.meta.url), "utf8");

test("the daemon log panel is present and receives bounded WebView batches", () => {
  assert.match(html, /id="logs-toggle"/);
  assert.match(html, /id="logs-host"[^>]*aria-label="Daemon logs"/);
  assert.match(main, /listen\("terminalai:logs"/);
  assert.match(main, /state\.logs = state\.logs\.slice\(-256\)/);
  assert.match(app, /const LOG_BATCH_INTERVAL: Duration = Duration::from_millis\(100\)/);
  assert.match(app, /VecDeque::<LogEntry>/);
  assert.match(app, /app\.emit\("terminalai:logs", batch\)/);
});

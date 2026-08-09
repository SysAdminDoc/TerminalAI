import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import {
  createRendererUtils,
  createTerminalOutput,
  escapeHtml,
  invokeArgs,
  terminalBytes,
} from "../src/rendererUtils.js";

test("renderer utility values keep bytes, escaping, and invocation shape stable", () => {
  assert.deepEqual([...terminalBytes([65, 66])], [65, 66]);
  const view = new Uint8Array([67, 68]).subarray(1);
  assert.deepEqual([...terminalBytes(view)], [68]);
  assert.equal(new TextDecoder().decode(terminalBytes("ok")), "ok");
  assert.equal(escapeHtml(`<tag attr="x">&'`), "&lt;tag attr=&quot;x&quot;&gt;&amp;&#039;");
  assert.deepEqual(invokeArgs({ cwd: "C:\\repo" }), {
    spec: { cwd: "C:\\repo" },
    configuredPath: null,
  });
});

test("render errors escape backend text and keep retry at the failing surface", () => {
  const dom = new JSDOM(`<main><div id="toast-region"></div><div id="body"></div></main>`);
  const { document } = dom.window;
  const utils = createRendererUtils({
    $: (id) => document.getElementById(id),
    document,
    t: (key) => key,
    requestAnimationFrame: (callback) => callback(),
    setTimeout: () => 0,
  });
  const body = document.getElementById("body");
  let retries = 0;

  utils.renderDataError(body, `<backend>`, "load", () => retries++);
  assert.match(body.innerHTML, /&lt;backend&gt;/);
  assert.match(body.innerHTML, /data-retry-action="load"/);
  assert.match(body.textContent, /button-retry/);
  body.querySelector("button").click();
  assert.equal(retries, 1);

  utils.renderGuarded(body, "Could not render", "render", () => retries++, () => {
    throw new Error("bad data");
  });
  assert.equal(body.querySelector('[role="alert"]').textContent.includes("bad data"), true);
});

test("terminal output ignores stale focus generations", () => {
  const writes = [];
  const state = {
    focused: "a",
    focusGeneration: 4,
    terminal: { write: (bytes) => writes.push([...bytes]) },
  };
  const { writeTerminalBytes } = createTerminalOutput({ state });
  writeTerminalBytes("stale", "b", 4);
  writeTerminalBytes("stale-generation", "a", 3);
  writeTerminalBytes("live", "a", 4);
  assert.deepEqual(writes, [[108, 105, 118, 101]]);
});

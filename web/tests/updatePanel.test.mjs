import assert from "node:assert/strict";
import test from "node:test";

import { JSDOM } from "jsdom";
import { createUpdatePanel } from "../src/updatePanel.js";

test("update panel keeps an available release visible and clears it on a new check", () => {
  const dom = new JSDOM(`
    <section id="update-result" class="view-hidden" role="status">
      <p id="update-result-message"></p>
    </section>
    <button id="update-check-button"><span></span></button>
  `);
  const { document } = dom.window;
  const panel = createUpdatePanel({
    $: (id) => document.getElementById(id),
    fallbackVersion: "0.23.0",
    invoke: async () => "0.23.0",
    showToast: () => {},
    state: {},
    t: (key) => key,
  });

  panel.showUpdateResult("TerminalAI v0.24.0 is available");
  assert.equal(document.getElementById("update-result").classList.contains("view-hidden"), false);
  assert.equal(document.getElementById("update-result-message").textContent, "TerminalAI v0.24.0 is available");
  panel.showUpdateResult(null);
  assert.equal(document.getElementById("update-result").classList.contains("view-hidden"), true);
  assert.equal(document.getElementById("update-result-message").textContent, "");
});

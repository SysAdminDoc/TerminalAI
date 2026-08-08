import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registrySource } from "./registrySource.mjs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const main = appSource();
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const app = appRustSource();
const daemon = readFileSync(
  new URL("../../crates/terminalai-daemon/src/lib.rs", import.meta.url),
  "utf8",
);
const registry = registrySource();

test("every daemon-wide limit is editable rather than environment-only", () => {
  for (const id of [
    "settings-max-live",
    "settings-default-budget",
    "settings-spend-ceiling",
    "settings-spend-window",
    "settings-memory-budget",
    "settings-memory-cap",
    "settings-max-processes",
  ]) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} has no field`);
  }
  assert.match(html, /id="settings-dialog"/);
  assert.match(html, /id="settings-toggle"/);
});

test("an empty field means no limit, not a limit of zero", () => {
  // Zero would ask the daemon for a ceiling of nothing; the two facts must not
  // agree by accident.
  assert.match(main, /function optionalNumber\(id\)/);
  assert.match(main, /if \(!raw\) return null;/);
});

test("applying settings does not require restarting the daemon", () => {
  assert.match(app, /fn set_admission\(/);
  assert.match(daemon, /Request::SetAdmission/);
  assert.match(registry, /pub fn set_admission\(&self, admission: AdmissionConfig\)/);
  // A limit is an admission policy, not a kill switch.
  assert.match(registry, /Sessions already running are\n    \/\/\/ untouched/);
  // The queue is re-evaluated so a raised cap takes effect immediately.
  assert.match(registry, /self\.drain_queue\(\);\n    \}\n\n    pub fn admission_config/);
});

test("the dialog says when a value came from the boot environment", () => {
  assert.match(daemon, /const ADMISSION_ENVIRONMENT: &\[&str\]/);
  assert.match(daemon, /pub from_environment: Vec<String>/);
  assert.match(main, /settings-from-environment/);
  assert.match(html, /id="settings-environment-note"/);
  assert.ok(/^settings-from-environment = /m.test(ftl));
});

test("the new commands are granted in every capability file", () => {
  for (const file of ["default.json", "wdio.json", "wdio-embedded.json"]) {
    const capability = readFileSync(
      new URL(`../../crates/terminalai-app/capabilities/${file}`, import.meta.url),
      "utf8",
    );
    for (const permission of ["allow-admission-config", "allow-set-admission"]) {
      assert.ok(
        capability.includes(permission),
        `${file} does not grant ${permission}; the ACL is checked at invoke time, so the failure would reach a user`,
      );
    }
  }
});

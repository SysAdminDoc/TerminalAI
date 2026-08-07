import assert from "node:assert/strict";
import test from "node:test";

import { checkForUpdates, isNewerVersion, versionTuple } from "../src/updateCheck.js";

test("a version is only newer when it actually is", () => {
  // A wrong answer here either nags about a release the operator already has,
  // or stays silent about one they do not.
  assert.ok(isNewerVersion("0.16.0", "0.15.0"));
  assert.ok(isNewerVersion("1.0.0", "0.99.99"));
  assert.ok(isNewerVersion("0.15.1", "0.15.0"));
  assert.ok(!isNewerVersion("0.15.0", "0.15.0"), "the same version is not newer");
  assert.ok(!isNewerVersion("0.14.9", "0.15.0"));
  // Numeric, not lexical: "0.9.0" must not beat "0.10.0".
  assert.ok(!isNewerVersion("0.9.0", "0.10.0"));
  assert.ok(isNewerVersion("0.10.0", "0.9.0"));
});

test("a tag that is not a version answers no rather than guessing", () => {
  assert.equal(versionTuple("nightly"), null);
  assert.equal(versionTuple(""), null);
  assert.equal(versionTuple(null), null);
  assert.deepEqual(versionTuple("v0.15.0"), [0, 15, 0]);
  assert.deepEqual(versionTuple("0.15.0-rc.1"), [0, 15, 0], "a prerelease suffix is ignored");
  assert.ok(!isNewerVersion("nightly", "0.15.0"), "an unreadable tag is never newer");
  assert.ok(!isNewerVersion("0.16.0", "nightly"), "an unreadable installed version claims nothing");
});

/** The collaborators `checkForUpdates` takes, recording what it did with them. */
function harness({ status = 200, body = {}, installed = "0.15.0" } = {}) {
  const span = { textContent: "" };
  const button = { disabled: false, querySelector: () => span };
  const toasts = [];
  const results = [];
  return {
    toasts,
    results,
    span,
    button,
    deps: {
      $: () => button,
      t: (key, args) => (args ? `${key}:${JSON.stringify(args)}` : key),
      invoke: async () => installed,
      state: {},
      showToast: (message, tone) => toasts.push({ message, tone }),
      showUpdateResult: (message) => results.push(message),
      fallbackVersion: "0.0.0",
      fetch: async () => ({
        status,
        ok: status >= 200 && status < 300,
        json: async () => body,
      }),
    },
  };
}

test("a newer release is reported where the operator can act on it", async () => {
  const { deps, toasts, results } = harness({ body: { tag_name: "v0.16.0" } });
  await checkForUpdates(deps);
  assert.equal(results.at(-1), 'update-available:{"latest":"0.16.0","current":"0.15.0"}');
  assert.deepEqual(toasts, [], "the actionable outcome does not expire in a toast");
});

test("being up to date is a toast, because it asks for nothing", async () => {
  const { deps, toasts, results } = harness({ body: { tag_name: "v0.15.0" } });
  await checkForUpdates(deps);
  assert.equal(toasts.at(-1).message, 'update-up-to-date:{"version":"0.15.0"}');
  assert.deepEqual(results, [null], "the result panel is cleared, not filled");
});

test("a release with no readable version is an error, not a silent pass", async () => {
  // Reporting "up to date" against a tag we could not parse would tell the
  // operator something we do not know.
  const { deps, toasts } = harness({ body: { tag_name: "nightly" } });
  await checkForUpdates(deps);
  assert.match(toasts.at(-1).message, /^update-failed:/);
  assert.match(toasts.at(-1).message, /update-invalid-release/);
});

test("a repository with no releases yet is not a failure", async () => {
  const { deps, toasts } = harness({ status: 404 });
  await checkForUpdates(deps);
  assert.equal(toasts.at(-1).message, 'update-newest:{"version":"0.15.0"}');
  assert.equal(toasts.at(-1).tone, "success");
});

test("the button is re-enabled and relabelled whatever happened", async () => {
  for (const options of [{ body: { tag_name: "v0.16.0" } }, { status: 500 }, { status: 404 }]) {
    const { deps, button, span } = harness(options);
    await checkForUpdates(deps);
    assert.equal(button.disabled, false, `left disabled after ${JSON.stringify(options)}`);
    assert.equal(span.textContent, "button-check-updates");
  }
});

test("a second click while a check is running does nothing", async () => {
  const { deps, button } = harness({ body: { tag_name: "v0.16.0" } });
  button.disabled = true;
  await checkForUpdates(deps);
  assert.equal(button.disabled, true, "the in-flight check owns the button");
});

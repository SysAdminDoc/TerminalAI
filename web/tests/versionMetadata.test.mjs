import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const testsRoot = dirname(fileURLToPath(import.meta.url));
const webRoot = resolve(testsRoot, "..");
const packageJson = JSON.parse(readFileSync(resolve(webRoot, "package.json"), "utf8"));
const lockfile = JSON.parse(readFileSync(resolve(webRoot, "package-lock.json"), "utf8"));
const mainSource = readFileSync(resolve(webRoot, "src", "main.js"), "utf8");
const viteConfig = (await import("../vite.config.js")).default;

test("the update fallback is injected from the web manifest", () => {
  assert.equal(viteConfig.define.__APP_VERSION__, JSON.stringify(packageJson.version));
  assert.match(mainSource, /const FALLBACK_APP_VERSION = __APP_VERSION__/);
  assert.doesNotMatch(mainSource, /const FALLBACK_APP_VERSION = ["']/);
});

test("every exactly-pinned dependency is the version the lockfile resolved", () => {
  // `@xterm/addon-fit` was declared as 0.12.1 while the lockfile and
  // node_modules both held 0.11.0 — and 0.12.1 does not exist on npm, so
  // `npm ci` failed outright on a fresh clone. Nothing noticed, because every
  // suite runs against the node_modules already on disk: the installed tree is
  // correct and the manifest describing it is not.
  const root = lockfile.packages?.[""];
  assert.ok(root, "the lockfile has no root package entry");
  const declared = { ...packageJson.dependencies, ...packageJson.devDependencies };
  const drifted = Object.entries(declared)
    // Ranges are the lockfile's job to resolve; only exact pins can disagree.
    .filter(([, range]) => /^\d/.test(range))
    .filter(([name, pinned]) => {
      const resolved =
        lockfile.packages?.[`node_modules/${name}`]?.version ??
        root.dependencies?.[name] ??
        root.devDependencies?.[name];
      return resolved !== undefined && resolved !== pinned;
    });
  assert.deepEqual(drifted, [], "package.json pins a version the lockfile does not have");
});

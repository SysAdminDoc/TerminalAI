import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const testsRoot = dirname(fileURLToPath(import.meta.url));
const webRoot = resolve(testsRoot, "..");
const packageJson = JSON.parse(readFileSync(resolve(webRoot, "package.json"), "utf8"));
const mainSource = readFileSync(resolve(webRoot, "src", "main.js"), "utf8");
const viteConfig = (await import("../vite.config.js")).default;

test("the update fallback is injected from the web manifest", () => {
  assert.equal(viteConfig.define.__APP_VERSION__, JSON.stringify(packageJson.version));
  assert.match(mainSource, /const FALLBACK_APP_VERSION = __APP_VERSION__/);
  assert.doesNotMatch(mainSource, /const FALLBACK_APP_VERSION = ["']/);
});

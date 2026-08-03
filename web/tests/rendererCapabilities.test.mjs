import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the WebGL renderer is loaded rather than left on the DOM fallback", () => {
  // xterm 6.0 dropped addon-canvas, so with no WebGL addon the DOM renderer was
  // the only one available — the slowest of the three.
  assert.equal(pkg.dependencies["@xterm/addon-webgl"], "0.19.0");
  assert.match(main, /import \{ WebglAddon \} from "@xterm\/addon-webgl";/);
  assert.match(main, /state\.webglAddon = useWebglRenderer\(state\.terminal\)/);
});

test("WebGL failure falls back to the DOM renderer instead of blanking the pane", () => {
  // Three distinct failure points: constructing the addon, creating the context
  // inside loadAddon, and losing the context later to a driver reset.
  assert.match(main, /function useWebglRenderer\(terminal\) \{/);
  const body = main.slice(main.indexOf("function useWebglRenderer"));
  const fn = body.slice(0, body.indexOf("\nfunction setupTerminal"));
  assert.equal((fn.match(/catch \(error\)/g) ?? []).length, 2, "both throw sites are guarded");
  assert.match(fn, /addon\.onContextLoss\(\(\) => \{/);
  assert.match(fn, /addon\.dispose\(\);/);
  assert.ok(
    fn.includes("return null"),
    "a failed renderer must leave state.webglAddon null, not a dead addon",
  );
});

test("the renderer is attached before a WebGL context is requested", () => {
  // loadAddon creates the context against the attached element; calling it
  // before open() throws and would silently drop us to the DOM renderer.
  const open = main.indexOf('state.terminal.open($("terminal-host"))');
  const webgl = main.indexOf("state.webglAddon = useWebglRenderer");
  assert.ok(open > 0 && webgl > open, "useWebglRenderer must follow terminal.open");
});

test("xterm measures character widths against the same table as the Rust grid", () => {
  // The Rust grid uses `unicode-width`; xterm defaults to Unicode 6. Left
  // mismatched, the two disagree about where a line wraps and the status
  // inferred from the Rust grid stops describing what the pane shows.
  assert.equal(pkg.dependencies["@xterm/addon-unicode11"], "0.9.0");
  assert.match(main, /state\.terminal\.unicode\.activeVersion = "11";/);
  // The addon is proposed API, so this had to flip from false.
  assert.match(main, /allowProposedApi: true,/);
});

test("OSC 8 hyperlinks are activated, and the scheme check is not done in the renderer", () => {
  assert.match(main, /linkHandler: \{/);
  assert.match(main, /activate: \(event, uri\) => \{/);
  assert.match(main, /event\.preventDefault\(\);/);
  assert.match(main, /invoke\("open_external_url", \{ url: uri \}\)/);
  // Agent output is untrusted; the allowlist lives in Rust so the renderer is
  // not the only thing between it and ShellExecute.
  assert.doesNotMatch(main, /uri\.startsWith\("https?:/);
});

test("a refused link is surfaced, never swallowed", () => {
  const fn = main.slice(main.indexOf("async function openSessionLink"));
  const body = fn.slice(0, fn.indexOf("\n/// Swap the DOM renderer"));
  assert.match(body, /catch \(error\) \{\s*showToast\(String\(error\)\);/);
});

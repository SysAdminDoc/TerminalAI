import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { createTerminalPane } from "../src/terminalPane.js";

const main = appSource();
const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

test("the WebGL renderer is loaded rather than left on the DOM fallback", () => {
  // xterm 6.0 dropped addon-canvas, so with no WebGL addon the DOM renderer was
  // the only one available — the slowest of the three.
  assert.equal(pkg.dependencies["@xterm/addon-webgl"], "0.19.0");
  assert.match(main, /import \{ WebglAddon \} from "@xterm\/addon-webgl";/);
  assert.match(main, /state\.webglAddon = useWebglRenderer\(state\.terminal\)/);
});

// Drive the real function rather than grepping it.
//
// This was a source-grep — count the `catch (error)` blocks, look for
// `dispose`, look for `return null` — and it broke the moment the pane moved
// into its own module, because the slice it took ended at a function that is
// now indented inside a factory. The failure was correct: it was a proxy for
// behaviour, and the extraction is what made the behaviour reachable. All three
// failure points are now exercised for real.
function pane(WebglAddon) {
  const state = { terminal: null, webglAddon: null };
  const { useWebglRenderer } = createTerminalPane({
    $: () => null,
    state,
    invoke: async () => null,
    showToast: () => {},
    t: (key) => key,
    scheduleFit: () => {},
    renderFindCount: () => {},
    Terminal: class {},
    FitAddon: class {},
    SearchAddon: class {},
    Unicode11Addon: class {},
    WebglAddon,
    DEFAULT_COLS: 120,
    DEFAULT_ROWS: 40,
  });
  return { state, useWebglRenderer };
}

test("a WebGL addon that cannot be constructed leaves the DOM renderer in place", () => {
  const { useWebglRenderer } = pane(
    class {
      constructor() {
        throw new Error("no GPU path");
      }
    },
  );
  assert.equal(useWebglRenderer({ loadAddon: () => {} }), null);
});

test("a WebGL context that cannot be created disposes the addon rather than keeping a dead one", () => {
  let disposed = false;
  const { useWebglRenderer } = pane(
    class {
      onContextLoss() {}
      dispose() {
        disposed = true;
      }
    },
  );
  const terminal = {
    loadAddon() {
      // loadAddon is where context creation actually happens.
      throw new Error("context creation failed");
    },
  };
  assert.equal(useWebglRenderer(terminal), null);
  assert.ok(disposed, "a failed addon must be disposed, not left attached");
});

test("losing the context later returns the pane to the DOM renderer", () => {
  let lose = null;
  let disposed = false;
  const { state, useWebglRenderer } = pane(
    class {
      onContextLoss(handler) {
        lose = handler;
      }
      dispose() {
        disposed = true;
      }
    },
  );
  const addon = useWebglRenderer({ loadAddon: () => {} });
  assert.ok(addon, "a working renderer is returned");
  state.webglAddon = addon;
  assert.ok(lose, "the loss handler is registered");
  // A driver reset or a GPU process crash, after everything already worked.
  lose();
  assert.ok(disposed, "the addon is disposed on context loss");
  assert.equal(state.webglAddon, null, "state must not keep a dead addon");
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

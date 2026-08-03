import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const main = readFileSync(new URL("../src/main.js", import.meta.url), "utf8");
const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const config = JSON.parse(
  readFileSync(new URL("../../crates/terminalai-app/tauri.conf.json", import.meta.url), "utf8"),
);

test("the CSP names the directives that do not inherit from default-src", () => {
  const csp = config.app.security.csp;
  // base-uri and form-action fall back to nothing, not to default-src, so a
  // policy that omits them leaves an injected <base> or <form> unconstrained.
  for (const directive of ["base-uri 'none'", "form-action 'none'", "object-src 'none'"]) {
    assert.ok(csp.includes(directive), `CSP is missing ${directive}`);
  }
});

test("agent output cannot reach the system clipboard", () => {
  // xterm 6.0 added OSC 52 support, gated on the clipboard addon being loaded.
  // Terminal output is untrusted — it is whatever the agent and its tools print
  // — so writing it to the clipboard is a threat, not a feature.
  const deps = { ...pkg.dependencies, ...pkg.devDependencies };
  assert.ok(
    !Object.keys(deps).some((name) => name.includes("addon-clipboard")),
    "the xterm clipboard addon must not be a dependency",
  );
  assert.doesNotMatch(main, /ClipboardAddon|addon-clipboard/);
  // allowProposedApi stays off: it is what unlocks the unstable addon surface.
  assert.match(main, /allowProposedApi:\s*false/);
});

/** Every `${...}` in `source`, brace-balanced so nested template literals stay whole. */
function interpolations(source) {
  const holes = [];
  for (let index = 0; index < source.length - 1; index += 1) {
    if (source[index] !== "$" || source[index + 1] !== "{") continue;
    let depth = 0;
    for (let end = index + 1; end < source.length; end += 1) {
      if (source[end] === "{") depth += 1;
      else if (source[end] === "}") {
        depth -= 1;
        if (depth === 0) {
          holes.push(source.slice(index, end + 1));
          index = end;
          break;
        }
      }
    }
  }
  return holes;
}

test("every interpolation into a markup attribute is escaped", () => {
  // One unescaped attribute is enough to break out of it. Agent-derived values
  // reach these strings, so the rule is uniform rather than case-by-case: if it
  // lands inside quotes, it goes through escapeHtml.
  const attributes =
    main.match(/\b(?:title|aria-label|aria-keyshortcuts|class|href|src|data-[a-z-]+)="[^"]*"/g) ?? [];
  const unescaped = attributes
    .filter((attribute) => attribute.includes("${"))
    .filter((attribute) =>
      interpolations(attribute).some(
        (hole) =>
          !hole.includes("escapeHtml") &&
          // A ternary between two string literals introduces no external data.
          !/^\$\{[^{}]*\?\s*"[^"]*"\s*:\s*"[^"]*"\}$/.test(hole),
      ),
    );
  assert.deepEqual(unescaped, [], "unescaped interpolation inside an attribute");
});

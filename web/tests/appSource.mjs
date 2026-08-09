// The renderer's own source, as one string.
//
// Several tests assert things about how the app is wired that have no DOM to
// observe: that `allowProposedApi` is on for exactly one addon, that the WebGL
// context is requested only after the element is attached, that a link's scheme
// is checked in Rust rather than here. Those read the source.
//
// They used to read `main.js` directly, which breaks every time a seam is
// extracted — and the decomposition is deliberate ongoing work, so "the test
// broke because the code moved" was going to happen once per seam. Worse, it
// breaks in the direction that hides things: an assertion about the terminal
// pane silently stops covering the terminal pane the moment the pane is a
// different file.
//
// Reading every renderer module makes the assertions about the *renderer*
// rather than about one of its files, exactly as `registrySource.mjs` does for
// the Rust registry. Negative assertions get stronger this way rather than
// weaker: "nothing hard-codes the grid at 120x40" should be true of the whole
// frontend, not only of whichever file the code sits in today.
//
// `main.js` is first so tests that slice from an index find the shell's own
// text before the modules it delegates to.
//
// Line endings are normalized for the same reason `registrySource.mjs`
// normalizes them: Git checks this repository out with CRLF on Windows, and a
// pattern spanning two lines is not supposed to depend on how the file reached
// the disk.
//
// Not a `.test.mjs` file, so `node --test tests/*.test.mjs` does not run it.

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const SRC_DIR = fileURLToPath(new URL("../src/", import.meta.url));

/// `main.js` alone.
///
/// For the few assertions that are genuinely about the *shell* rather than the
/// renderer — chiefly "the shell must not re-implement what a module already
/// exports", which is exactly the duplication these extractions exist to
/// remove. Those must not see the module's own definition, so they cannot use
/// `appSource()`.
export function shellSource() {
  return readFileSync(SRC_DIR + "main.js", "utf8").replace(/\r\n/g, "\n");
}

/// One renderer module, for contracts that belong to an owning boundary.
///
/// Prefer this over `appSource()` whenever an assertion is about one feature.
/// The assembled source remains available only for genuinely cross-cutting
/// invariants such as the complete renderer's literal-attribute scan.
export function moduleSource(name) {
  if (!name || name.includes("\\") || name.includes("/")) {
    throw new Error(`invalid renderer module name: ${name}`);
  }
  const filename = name.endsWith(".js") ? name : `${name}.js`;
  return readFileSync(SRC_DIR + filename, "utf8").replace(/\r\n/g, "\n");
}

/// Every renderer module, concatenated, `main.js` first.
export function appSource() {
  const files = readdirSync(SRC_DIR)
    .filter((name) => name.endsWith(".js"))
    .sort((a, b) => {
      if (a === "main.js") return -1;
      if (b === "main.js") return 1;
      return a < b ? -1 : a > b ? 1 : 0;
    });
  if (!files.includes("main.js")) {
    throw new Error(`no main.js under ${SRC_DIR}`);
  }
  return files
    .map((name) => readFileSync(SRC_DIR + name, "utf8").replace(/\r\n/g, "\n"))
    .join("\n");
}

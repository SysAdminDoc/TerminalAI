import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SRC_DIR = fileURLToPath(new URL("../src/", import.meta.url));
const IMPORT = /@import\s+"([^"]+)"\s*;/g;

function readStylesheet(relativePath, stack = []) {
  const path = resolve(SRC_DIR, relativePath);
  if (stack.includes(path)) {
    throw new Error(`stylesheet import cycle: ${[...stack, path].join(" -> ")}`);
  }
  const source = readFileSync(path, "utf8").replace(/\r\n/g, "\n");
  return source.replace(IMPORT, (_match, imported) =>
    readStylesheet(imported, [...stack, path]),
  );
}

/// Read the CSS as the browser receives it after resolving the barrel imports.
/// Source assertions should cover the rendered cascade, not only the tiny entry file.
export function cssSource() {
  return readStylesheet("styles.css");
}

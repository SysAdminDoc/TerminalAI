// The registry's Rust source, as one string.
//
// Several frontend tests assert a cross-language contract: a constant, a status
// rule or a signature the UI depends on, read out of the Rust that enforces it.
// They used to read `registry.rs` directly, which broke the moment the registry
// became a directory — and broke silently in the sense that the Rust suite stayed
// green while every one of these assertions started throwing ENOENT.
//
// Reading the directory instead makes the assertions about the registry rather
// than about one of its files, so splitting it again costs nothing here.
//
// Not a `.test.mjs` file, so `node --test tests/*.test.mjs` does not run it.

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const REGISTRY_DIR = fileURLToPath(
  new URL("../../crates/terminalai-core/src/registry/", import.meta.url),
);

/// Every `.rs` file under `registry/`, concatenated in a stable order.
export function registrySource() {
  const files = readdirSync(REGISTRY_DIR)
    .filter((name) => name.endsWith(".rs"))
    .sort();
  if (files.length === 0) {
    throw new Error(`no Rust sources found under ${REGISTRY_DIR}`);
  }
  return files
    .map((name) => readFileSync(REGISTRY_DIR + name, "utf8"))
    .join("\n");
}

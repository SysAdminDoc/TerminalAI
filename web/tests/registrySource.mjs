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
// Line endings are normalized on the way out. These assertions are about what
// the Rust says, and Git checks this repository out with CRLF on Windows — so a
// pattern spanning two lines matched or failed depending on how the file
// reached the disk, which is not a fact about the contract being asserted. It
// bit once for real: a tool that rewrote a source file in text mode flipped it
// to CRLF and a passing assertion started failing with the code unchanged.
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
    .map((name) => readFileSync(REGISTRY_DIR + name, "utf8").replace(/\r\n/g, "\n"))
    .join("\n");
}

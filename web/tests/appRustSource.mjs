// The Tauri app's Rust source, as one string.
//
// Cross-language tests sometimes need to prove that the command registered by
// the shell is backed by the implementation that enforces it. Reading only
// `main.rs` made those tests fail as soon as a command family moved into a
// module, which is exactly the refactor those tests should permit.

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const APP_SRC_DIR = fileURLToPath(
  new URL("../../crates/terminalai-app/src/", import.meta.url),
);

/// Every Rust source file directly owned by the app, with `main.rs` first so
/// command-registration assertions still begin at the shell boundary.
export function appRustSource() {
  const files = readdirSync(APP_SRC_DIR)
    .filter((name) => name.endsWith(".rs"))
    .sort((a, b) => {
      if (a === "main.rs") return -1;
      if (b === "main.rs") return 1;
      return a < b ? -1 : a > b ? 1 : 0;
    });
  if (!files.includes("main.rs")) {
    throw new Error(`no main.rs under ${APP_SRC_DIR}`);
  }
  return files
    .map((name) => readFileSync(APP_SRC_DIR + name, "utf8").replace(/\r\n/g, "\n"))
    .join("\n");
}

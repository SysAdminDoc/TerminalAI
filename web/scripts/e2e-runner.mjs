import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptRoot = path.dirname(fileURLToPath(import.meta.url));
const webRoot = path.resolve(scriptRoot, "..");
const artifactRoot = process.env.TERMINALAI_E2E_ARTIFACTS ?? path.join(webRoot, "artifacts", "wdio");
mkdirSync(artifactRoot, { recursive: true });
const logPath = path.join(artifactRoot, "wdio.log");
const exitPath = path.join(artifactRoot, "exit-code");
const log = (chunk) => appendFileSync(logPath, chunk);

const child = spawn(process.execPath, [
  path.join(webRoot, "node_modules", "@wdio", "cli", "bin", "wdio.js"),
  "run",
  "wdio.conf.mjs",
], {
  cwd: webRoot,
  env: process.env,
  stdio: ["ignore", "pipe", "pipe"],
  windowsHide: true,
});
child.stdout.on("data", log);
child.stderr.on("data", log);
child.on("error", (error) => {
  log(`${error.stack ?? error}\n`);
  writeFileSync(exitPath, "1");
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  const exitCode = code ?? 1;
  log(`\n[terminalai-e2e] exit=${exitCode} signal=${signal ?? "none"}\n`);
  writeFileSync(exitPath, String(exitCode));
  process.exitCode = exitCode;
});

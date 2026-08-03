import path from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(webRoot, "..");
const appBinaryPath = path.join(repoRoot, "target", "release", "terminalai.exe");
const tauriDriverPath = path.join(process.env.USERPROFILE, ".cargo", "bin", "tauri-driver.exe");

if (process.env.TERMINALAI_E2E_LOCALAPPDATA) {
  process.env.LOCALAPPDATA = process.env.TERMINALAI_E2E_LOCALAPPDATA;
}
process.env.TAURI_AUTOMATION = "true";
process.env.TAURI_WEBVIEW_AUTOMATION = "true";

export const config = {
  runner: "local",
  specs: ["./tests/ui.e2e.mjs"],
  maxInstances: 1,
  services: [["@wdio/tauri-service", {
    appBinaryPath,
    driverProvider: "external",
    tauriDriverPath,
    captureBackendLogs: false,
    captureFrontendLogs: false,
    env: process.env,
  }]],
  capabilities: [{
    browserName: "tauri",
    "tauri:options": { application: appBinaryPath },
  }],
  logLevel: "warn",
  waitforTimeout: 10000,
  connectionRetryTimeout: 90000,
  connectionRetryCount: 2,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    timeout: 60000,
  },
};

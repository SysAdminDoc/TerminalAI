import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const visualIsolation = path.join(process.env.USERPROFILE, ".claude", "scripts", "visual-isolation.ps1");
const artifacts = path.join(webRoot, "artifacts", "wdio");
const localAppData = path.join(webRoot, ".e2e-localappdata");
const runner = path.join(webRoot, "scripts", "e2e-runner.mjs");
const hostSource = path.join(webRoot, "scripts", "isolated-wdio-host.cs");
const hostBinary = path.join(webRoot, "artifacts", "isolated-wdio-host.exe");
const launchDesktop = `TerminalAIWdio${process.pid}`;
const tauriConfig = JSON.stringify({ app: { withGlobalTauri: true } });

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? repoRoot,
    env: options.env ?? process.env,
    encoding: "utf8",
    stdio: options.stdio ?? "inherit",
    timeout: options.timeout,
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
    throw new Error(`${command} ${args.join(" ")} exited with ${result.status}${output ? `\n${output}` : ""}`);
  }
  return result;
}

function taskkill(pid) {
  if (!pid) return;
  spawnSync("taskkill.exe", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
}

function readLaunchJson(output) {
  const match = output.match(/\{"processId":\d+,"desktop":"[^"]+"[^\r\n]*\}/);
  assert.ok(match, `visual-isolation launch did not return a process record:\n${output}`);
  return JSON.parse(match[0]);
}

function readBoundsJson(output) {
  const lines = output.split(/\r?\n/).reverse();
  for (const line of lines) {
    try {
      const value = JSON.parse(line.trim());
      if (["x", "y", "width", "height"].every((key) => Number.isInteger(value[key]))) return value;
    } catch {}
  }
  throw new Error(`visual-isolation ensure did not return display bounds:\n${output}`);
}

function processExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function sleep(milliseconds) {
  const buffer = new SharedArrayBuffer(4);
  Atomics.wait(new Int32Array(buffer), 0, 0, milliseconds);
}

function waitForRunnerAndVerifyPlacement(pid, placementPath, desktopName) {
  const deadline = Date.now() + 180_000;
  let placementVerified = false;
  while (processExists(pid)) {
    if (!placementVerified && existsSync(placementPath)) {
      const placement = JSON.parse(readFileSync(placementPath, "utf8"));
      run("pwsh.exe", [
        "-NoLogo",
        "-NoProfile",
        "-File",
        visualIsolation,
        "verify",
        "-ProcessId",
        String(placement.processId),
        "-DesktopName",
        desktopName,
      ], { cwd: repoRoot });
      placementVerified = true;
    }
    if (Date.now() >= deadline) throw new Error(`isolated E2E runner exceeded timeout; placementVerified=${placementVerified}`);
    sleep(100);
  }
  assert.ok(placementVerified, "isolated E2E runner exited before the app placement proof was recorded");
}

mkdirSync(artifacts, { recursive: true });
rmSync(artifacts, { recursive: true, force: true });
mkdirSync(artifacts, { recursive: true });
rmSync(path.join(artifacts, "exit-code"), { force: true });
rmSync(path.join(artifacts, "wdio.log"), { force: true });
mkdirSync(localAppData, { recursive: true });
let launched;
let testStatus = 1;
try {
  run(process.execPath, [path.join(webRoot, "node_modules", "vite", "bin", "vite.js"), "build"], {
    cwd: webRoot,
    env: { ...process.env, VITE_TERMINALAI_WDIO: "1" },
  });
  // `custom-protocol` is not optional here: without it the app is a dev shell
  // pointed at `devUrl`, nothing serves that during a test run, and the window
  // comes up empty -- which reads as every assertion in the spec failing rather
  // than as a build that loaded no frontend.
  run("cargo", ["build", "--release", "--workspace", "--features", "terminalai-app/wdio,terminalai-app/custom-protocol"], {
    cwd: repoRoot,
    env: { ...process.env, TAURI_CONFIG: tauriConfig },
  });
  const ensure = run("pwsh.exe", ["-NoLogo", "-NoProfile", "-File", visualIsolation, "ensure"], { cwd: repoRoot, stdio: "pipe" });
  const boundsOutput = `${ensure.stdout ?? ""}\n${ensure.stderr ?? ""}`;
  console.log(boundsOutput.trim());
  const bounds = readBoundsJson(boundsOutput);
  const placementPath = path.join(artifacts, "placement.json");
  const csc = path.join(process.env.WINDIR ?? "C:\\Windows", "Microsoft.NET", "Framework64", "v4.0.30319", "csc.exe");
  run(csc, [
    "/nologo",
    "/target:winexe",
    `/out:${hostBinary}`,
    "/reference:System.dll",
    "/reference:System.Windows.Forms.dll",
    hostSource,
  ], { cwd: webRoot });

  const launch = run("pwsh.exe", [
    "-NoLogo",
    "-NoProfile",
    "-File",
    visualIsolation,
    "launch",
    "-DesktopName",
    launchDesktop,
    "-FilePath",
    hostBinary,
  ], {
    cwd: webRoot,
    env: {
      ...process.env,
      TERMINALAI_E2E_ARTIFACTS: artifacts,
      TERMINALAI_E2E_LOCALAPPDATA: localAppData,
      TERMINALAI_E2E_NODE: process.execPath,
      TERMINALAI_E2E_RUNNER: runner,
      TERMINALAI_E2E_APP_BINARY: path.join(repoRoot, "target", "release", "terminalai.exe"),
      TERMINALAI_E2E_PLACEMENT: placementPath,
      TERMINALAI_E2E_DISPLAY_X: String(bounds.x),
      TERMINALAI_E2E_DISPLAY_Y: String(bounds.y),
      TERMINALAI_E2E_DISPLAY_WIDTH: String(bounds.width),
      TERMINALAI_E2E_DISPLAY_HEIGHT: String(bounds.height),
    },
    stdio: "pipe",
    timeout: 120000,
  });
  const launchOutput = `${launch.stdout ?? ""}\n${launch.stderr ?? ""}`;
  launched = readLaunchJson(launchOutput);
  waitForRunnerAndVerifyPlacement(launched.processId, placementPath, launched.desktop);
  const exitPath = path.join(artifacts, "exit-code");
  assert.ok(existsSync(exitPath), "isolated E2E runner did not write an exit code");
  testStatus = Number.parseInt(readFileSync(exitPath, "utf8"), 10);
  assert.equal(testStatus, 0, readFileSync(path.join(artifacts, "wdio.log"), "utf8"));
} catch (error) {
  if (launched?.processId) taskkill(launched.processId);
  const logPath = path.join(artifacts, "wdio.log");
  const details = existsSync(logPath) ? `\n${readFileSync(logPath, "utf8")}` : "";
  console.error(`${error.stack ?? error}${details}`);
  process.exitCode = 1;
} finally {
  if (launched?.processId) {
    try {
      run("pwsh.exe", ["-NoLogo", "-NoProfile", "-File", visualIsolation, "verify", "-ProcessId", String(launched.processId), "-DesktopName", launched.desktop], { cwd: repoRoot, stdio: "inherit" });
      run("pwsh.exe", ["-NoLogo", "-NoProfile", "-File", visualIsolation, "sweep", "-ProcessId", String(launched.processId), "-DesktopName", launched.desktop], { cwd: repoRoot, stdio: "inherit" });
    } catch (error) {
      console.warn(`private desktop already closed during cleanup: ${error.message}`);
    }
  }
  rmSync(localAppData, { recursive: true, force: true });
  rmSync(artifacts, { recursive: true, force: true });
}

process.exitCode = testStatus;

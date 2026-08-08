import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appRustSource } from "./appRustSource.mjs";

const app = appRustSource();

test("slow Tauri commands are async and share the blocking executor", () => {
  for (const name of [
    "external_sessions",
    "land_session",
    "start_work_run",
    "approve_flagged_project",
    "skip_work_project",
    "set_work_run_paused",
    "list_projects",
    "scan_projects",
    "preflight_report",
  ]) {
    assert.match(app, new RegExp(`async fn ${name}\\b`), `${name} must not run on the UI thread`);
  }
  assert.match(app, /async fn run_blocking<T, F>/);
  assert.match(app, /tauri::async_runtime::spawn_blocking\(task\)/);
  assert.match(app, /run_blocking\("start_work_run"/);
  assert.match(app, /run_blocking\("land_session"/);
  assert.match(app, /run_blocking\("preflight_report"/);
});

import assert from "node:assert/strict";
import test from "node:test";

import { createWorkspacePages } from "../src/workspacePages.js";

test("workspace pages expose every shell-owned page handler", () => {
  const pages = createWorkspacePages(new Proxy({}, { get: () => undefined }));
  assert.equal(typeof pages.openApprovals, "function");
  assert.equal(typeof pages.renderApprovalInbox, "function");
  assert.equal(typeof pages.openProjects, "function");
});

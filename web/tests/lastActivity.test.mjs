import assert from "node:assert/strict";
import test from "node:test";

import { rateLimitedLabel } from "../src/rateLimit.js";
import { createSessionStatus } from "../src/sessionStatus.js";

const translate = (key) => key;
const { lastActivity } = createSessionStatus({ t: translate, rateLimitedLabel });

test("the row prefers the transcript message over the raw pty tail", () => {
  // `last_line` is the tail of a rendered TUI, so a redraw leaves box-drawing
  // characters and cursor moves in it. The transcript carries what the agent
  // actually said.
  assert.equal(
    lastActivity({ last_message: "agent said this", last_line: "raw terminal redraw" }),
    "agent said this",
  );
});

test("an empty transcript message does not blank the row", () => {
  // A whitespace-only message must not win over a pty tail that has content.
  assert.equal(lastActivity({ last_message: "  \n", last_line: "raw terminal tail" }), "raw terminal tail");
});

test("a row without either activity signal names the absence of output", () => {
  assert.equal(lastActivity({ last_message: "", last_line: "" }), "empty-no-output");
  assert.equal(lastActivity(undefined), "empty-no-output");
});

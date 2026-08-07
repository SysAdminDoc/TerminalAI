import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";

const main = appSource();

test("the row prefers the transcript message over the raw pty tail", () => {
  // `last_line` is the tail of a rendered TUI, so a redraw leaves box-drawing
  // characters and cursor moves in it. The transcript carries what the agent
  // actually said.
  assert.match(main, /function lastActivity\(session\) \{/);
  const fn = main.slice(main.indexOf("function lastActivity"));
  const body = fn.slice(0, fn.indexOf("\nfunction lifecycleLabel"));
  assert.match(body, /const message = session\?\.last_message;/);
  assert.match(body, /if \(typeof message === "string" && message\.trim\(\)\) return message;/);
  // Falls back rather than blanking: an empty row is worse than an ugly one.
  assert.match(body, /return session\?\.last_line \|\| t\("empty-no-output"\);/);
});

test("nothing reads last_line directly any more", () => {
  // A direct read would silently keep showing escape sequences on whichever
  // surface was missed.
  // Counts optional chaining too — `session?.last_line` is the form the
  // fallback actually uses, and a regex that missed it would pass vacuously.
  const direct = [...main.matchAll(/session\??\.last_line/g)];
  assert.equal(
    direct.length,
    1,
    `only the fallback inside lastActivity may read last_line, found ${direct.length}`,
  );
});

test("an empty transcript message does not blank the row", () => {
  // A whitespace-only message must not win over a pty tail that has content.
  const fn = main.slice(main.indexOf("function lastActivity"));
  assert.match(fn.slice(0, 400), /message\.trim\(\)/);
});

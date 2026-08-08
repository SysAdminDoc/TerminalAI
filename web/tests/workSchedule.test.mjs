import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appSource } from "./appSource.mjs";
import { appRustSource } from "./appRustSource.mjs";

import {
  REPEAT_CHOICES,
  countdown,
  lastFiringMessage,
  missedMessage,
  scheduleStatus,
  scheduleTimeMs,
} from "../src/workSchedule.js";

const main = appSource();
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const ftl = readFileSync(new URL("../src/i18n/terminalai.ftl", import.meta.url), "utf8");
const app = appRustSource();
const workflow = readFileSync(
  new URL("../../crates/terminalai-app/src/workflows.rs", import.meta.url),
  "utf8",
);
const schedule = readFileSync(
  new URL("../../crates/terminalai-core/src/schedule.rs", import.meta.url),
  "utf8",
);

const at = (seconds) => ({ secs_since_epoch: seconds, nanos_since_epoch: 0 });

test("a time this window cannot read produces no countdown at all", () => {
  // The alternative is a confident "next run in 56 years" from a missing field,
  // which reads exactly like a schedule that is working.
  assert.equal(scheduleTimeMs(undefined), null);
  assert.equal(scheduleTimeMs({}), null);
  assert.equal(scheduleTimeMs({ secs_since_epoch: "soon" }), null);
  assert.equal(scheduleTimeMs(at(90)), 90_000);
  assert.equal(countdown(null, 0), null);
});

test("the countdown names the coarsest unit that still describes the wait", () => {
  const now = 1_000_000_000;
  assert.deepEqual(countdown(now + 20 * 60 * 1000, now), {
    key: "work-schedule-next-minutes",
    args: { minutes: 20 },
  });
  assert.deepEqual(countdown(now + 3 * 3600 * 1000, now), {
    key: "work-schedule-next-hours",
    args: { hours: 3 },
  });
  assert.deepEqual(countdown(now + 3 * 24 * 3600 * 1000, now), {
    key: "work-schedule-next-days",
    args: { days: 3 },
  });
});

test("a firing that is due or overdue says so rather than counting backwards", () => {
  // Overdue is ordinary: the window was closed, or the check runs once a
  // minute. "In -4 minutes" would read as a bug in the schedule itself.
  const now = 1_000_000_000;
  assert.deepEqual(countdown(now, now), { key: "work-schedule-due", args: {} });
  assert.deepEqual(countdown(now - 90 * 60 * 1000, now), {
    key: "work-schedule-due",
    args: {},
  });
});

test("a firing that did nothing is reported with its reason", () => {
  // A schedule that mentions only its successes is the same as one that says
  // nothing: the operator still has to go and check.
  assert.equal(lastFiringMessage(undefined), null);
  assert.deepEqual(lastFiringMessage({ result: { kind: "started", projects: 4 } }), {
    key: "work-schedule-last-started",
    args: { count: 4 },
  });
  assert.deepEqual(
    lastFiringMessage({ result: { kind: "skipped", reason: "the previous run was still going" } }),
    {
      key: "work-schedule-last-skipped",
      args: { reason: "the previous run was still going" },
    },
  );
  // A result shape this build does not know is not rendered as a success.
  assert.equal(lastFiringMessage({ result: { kind: "something-new" } }), null);
});

test("occurrences that were missed are reported, never silently made up for", () => {
  assert.equal(missedMessage({ missed: 0 }), null);
  assert.equal(missedMessage({}), null);
  assert.deepEqual(missedMessage({ missed: 11 }), {
    key: "work-schedule-missed",
    args: { count: 11 },
  });
});

test("a held schedule shows that it is held rather than a next-run time", () => {
  const now = 1_000_000_000;
  assert.equal(scheduleStatus(null, now), null);
  const held = scheduleStatus(
    { paused: true, next_due: at(now / 1000 + 3600), history: [] },
    now,
  );
  assert.deepEqual(held, [{ key: "work-schedule-paused", args: {} }]);

  const running = scheduleStatus(
    {
      paused: false,
      next_due: at(now / 1000 + 3600),
      history: [{ result: { kind: "started", projects: 2 }, missed: 1 }],
    },
    now,
  );
  assert.deepEqual(running.map((part) => part.key), [
    "work-schedule-next-hours",
    "work-schedule-last-started",
    "work-schedule-missed",
  ]);
});

test("every message this view can produce exists in the catalog", () => {
  const keys = [
    "work-repeat",
    "work-repeat-off",
    "work-schedule-pause",
    "work-schedule-resume",
    "work-schedule-paused",
    "work-schedule-due",
    "work-schedule-next-minutes",
    "work-schedule-next-hours",
    "work-schedule-next-days",
    "work-schedule-last-started",
    "work-schedule-last-skipped",
    "work-schedule-missed",
    ...REPEAT_CHOICES.map((choice) => choice.key),
  ];
  for (const key of new Set(keys)) {
    assert.ok(new RegExp(`^${key} = `, "m").test(ftl), `${key} has no catalog message`);
  }
});

test("the schedule's controls are wired to the commands the backend registers", () => {
  assert.match(html, /id="work-repeat-select"/);
  assert.match(html, /id="work-schedule-status"/);
  assert.match(main, /invoke\("work_schedule"\)/);
  assert.match(main, /invoke\("set_work_schedule", \{/);
  assert.match(main, /invoke\("set_work_schedule_paused", \{ paused \}\)/);
  assert.match(main, /invoke\("clear_work_schedule"\)/);
  for (const command of [
    "work_schedule",
    "set_work_schedule",
    "set_work_schedule_paused",
    "clear_work_schedule",
  ]) {
    assert.ok(
      app.includes(`\n            ${command},`),
      `${command} is invoked but not registered in generate_handler!`,
    );
  }
});

test("a scheduled firing goes through the same path as the button beside it", () => {
  // The whole safety argument: dirty trees, admission, the spend ceiling and
  // the expired-credential hold are enforced by the on-demand path, so a
  // scheduled run must not have a second launch path of its own.
  const firing = workflow.slice(workflow.indexOf("fn scheduled_firing"));
  const body = firing.slice(0, firing.indexOf("\npub(crate) fn finish_work_run_session"));
  assert.match(body, /start_work_run_with\(/);
  assert.doesNotMatch(body, /Request::Launch/);
  assert.match(body, /previous_run_blocking\(/);
});

test("missed occurrences are skipped in the core, not queued up in the window", () => {
  // A laptop closed for a weekend must not wake up owing forty-two runs, each
  // landing on the uncommitted work of the one before it.
  assert.match(schedule, /pub fn advance_past/);
  assert.match(schedule, /fn a_weekend_asleep_owes_one_run_not_forty_two/);
  assert.match(app, /schedule\.advance_past\(now\)/);
});

// The drawer's acceptance-criteria checklist is folded from journal events
// (ptask_core::criteria). Runs the shipped foldCriteria() without a DOM, on
// both payload shapes (pt serve: JSON string; sidecar: object).

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");
const body = shell.match(/\nfunction foldCriteria\(events\)\{[\s\S]*?\n\}\n/);
assert.ok(body, "foldCriteria is in index.html");
const foldCriteria = new Function(`${body[0]}; return foldCriteria;`)();

// The events API is newest first.
const ev = (event_type, payload, actor = "hal") => ({ event_type, payload, actor });
const newestFirst = (...events) => events.reverse();

test("added, checked, unchecked and removed fold to the current checklist", () => {
  const got = foldCriteria(newestFirst(
    ev("task.criterion_added", JSON.stringify({ n: 1, text: "CI green" })),
    ev("task.criterion_added", { n: 2, text: "deployed" }),
    ev("task.criterion_added", { n: 3, text: "read back" }),
    ev("task.criterion_checked", { n: 1 }, "grok"),
    ev("task.criterion_checked", JSON.stringify({ n: 2 })),
    ev("task.criterion_unchecked", { n: 2 }),
    ev("task.criterion_removed", { n: 3 }),
    ev("task.updated", { status: "pending" }),
  ));
  assert.deepEqual(got, [
    { n: 1, text: "CI green", done: true, by: "grok" },
    { n: 2, text: "deployed", done: false, by: null },
  ]);
});

test("a recurring advance resets every check", () => {
  const got = foldCriteria(newestFirst(
    ev("task.criterion_added", { n: 1, text: "restore verified" }),
    ev("task.criterion_checked", { n: 1 }),
    ev("task.criteria_reset", {}),
  ));
  assert.equal(got[0].done, false);
});

test("the drawer has a hidden-until-needed criteria block, text escaped", () => {
  assert.match(shell, /<div class="dcrit" id="d-crit-wrap" hidden>/);
  assert.match(shell, /\$\{esc\(c\.n\)\}\. \$\{esc\(c\.text\)\}/);
});

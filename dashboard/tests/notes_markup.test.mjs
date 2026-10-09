// The drawer's Notes trail is rendered from journal events. pt serve sends each
// event's payload as a JSON string and the sidecar as an object; both must work,
// and a note is untrusted text. These checks run the shipped renderNotes()
// without a DOM; the browser half lives in tests/e2e/notes-e2e.mjs.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");

function load() {
  const esc = shell.match(/const esc=s=>[^\n]*\n/);
  const body = shell.match(/const NOTE_KIND=[\s\S]*?\nfunction renderNotes\(events\)\{[\s\S]*?\n\}\n/);
  assert.ok(esc && body, "esc and renderNotes are present in index.html");
  return new Function(`${esc[0]}${body[0]}; return renderNotes;`)();
}

const ev = (event_type, payload, actor = "hal", ts = "2026-10-09T15:59:00+01:00") => ({ event_type, payload, actor, ts });

test("notes render from string payloads (pt serve) and object payloads (sidecar)", () => {
  const renderNotes = load();
  const html = renderNotes([
    ev("task.completed", JSON.stringify({ note: "CI green on abc123" }), "shell"),
    ev("task.noted", { note: "rails arrived" }),
    ev("task.created", { title: "no note here" }),
    ev("task.updated", "not json"),
  ]);
  assert.match(html, /CI green on abc123/);
  assert.match(html, /rails arrived/);
  assert.match(html, /<span class="kind">done<\/span>/, "a closing note is labelled with its close");
  assert.doesNotMatch(html, /no note here/);
  assert.equal((html.match(/class="dnote"/g) || []).length, 2);
});

test("a note is text, never markup", () => {
  const renderNotes = load();
  const html = renderNotes([ev("task.noted", { note: '<img src=x onerror="alert(1)">' }, '<b>x</b>')]);
  assert.doesNotMatch(html, /<img/);
  assert.doesNotMatch(html, /<b>x<\/b>/);
  assert.match(html, /&lt;img src=x onerror=&quot;alert\(1\)&quot;&gt;/);
});

test("no notes is a designed empty state", () => {
  assert.match(load()([ev("task.created", "{}")]), /no notes yet/);
});

test("the done dialog and the drawer carry their note fields", () => {
  assert.match(shell, /<textarea id="confirm-note"[^>]*maxlength="4000"/);
  assert.match(shell, /<label for="confirm-note">/);
  assert.match(shell, /<form class="dnote-form" id="d-note-form">/);
  assert.match(shell, /\/note',\{method:'POST'/);
});

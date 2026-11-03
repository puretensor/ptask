// The flux popover's per-actor block: rendered from /api/stats by_actor, text
// only, with a positive net (the backlog grew) marked. Runs the shipped
// fluxActorsHtml() without a DOM.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");

function load(win = "24h") {
  const esc = shell.match(/const esc=s=>[^\n]*\n/);
  const names = shell.match(/const FLUX_NAMES=\{[^\n]*\};\n/);
  const body = shell.match(/\nfunction fluxActorsHtml\(fd\)\{[\s\S]*?\n\}\n/);
  assert.ok(esc && names && body, "esc, FLUX_NAMES and fluxActorsHtml are in index.html");
  return new Function(`${esc[0]}${names[0]}let _fluxWin=${JSON.stringify(win)};${body[0]}; return fluxActorsHtml;`)();
}

test("each actor shows opened, closed and net; a positive net is flagged", () => {
  const html = load()({ by_actor: [
    { actor: "hal", created: 11, reopened: 0, done: 2, dismissed: 0, net: 9 },
    { actor: "shell", created: 0, reopened: 1, done: 3, dismissed: 1, net: -3 },
  ] });
  assert.match(html, /by actor · 24 hours/);
  assert.match(html, /<span class="who">hal<\/span><span>\+11\/−2<\/span><span class="net up"[^>]*>\+9</);
  assert.match(html, /<span class="who">shell<\/span><span>\+1\/−4<\/span><span class="net down"[^>]*>-3</);
});

test("no split, no block; actor names are text", () => {
  assert.equal(load()({}), "");
  assert.equal(load()({ by_actor: [] }), "");
  const html = load()({ by_actor: [{ actor: "<img src=x onerror=1>", created: 1, net: 1 }] });
  assert.doesNotMatch(html, /<img/);
});

test("the range picker appends the block for the selected window", () => {
  assert.match(shell, /\}\)\.join\(''\)\+fluxActorsHtml\(bw\[_fluxWin\]\);/);
});

function renderRecent(mode, tasks) {
  const body = shell.match(/\nfunction renderRecent\(tasks\)\{[\s\S]*?\n\}\nlet _laneScroll/);
  assert.ok(body, "recent activity renderer exists");
  const nodes = { "#rc-ct": {}, "#recent": {} };
  const esc = (s) => String(s).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll('"', "&quot;");
  const fn = new Function("$", "_recentMode", "_domain", "DNAME", "inDom", "relTime", "esc", "ptrigger", "originTag", "domainTag",
    body[0].replace(/let _laneScroll$/, "") + ";return renderRecent;");
  fn((s) => nodes[s], mode, "all", {}, () => "", (s) => "relative:" + s, esc, () => "", () => "", () => "")(tasks);
  return nodes;
}

test("Closed is the visible default and uses closure time rather than creation or edits", () => {
  assert.match(shell, /id="recent-closed" aria-selected="true"/);
  assert.match(shell, /role="tabpanel" aria-labelledby="recent-closed"/);
  const view = renderRecent("closed", [{ id: "a", title: "old task", status: "done", priority: 2,
    created_at: "2020-01-01T00:00:00Z", updated_at: "2099-01-01T00:00:00Z", closed_at: "2026-11-03T14:48:00.000Z" }]);
  assert.match(view["#rc-ct"].textContent, /newest closure first/);
  assert.match(view["#recent"].innerHTML, /closed 2026-11-03 14:48:00 UTC/);
  assert.match(view["#recent"].innerHTML, /<b>14:48:00<\/b><small>2026-11-03<\/small>/);
  assert.doesNotMatch(view["#recent"].innerHTML, /2020|2099/);
});

test("unknown closure times are explicit and Added retains creation time", () => {
  const task = { id: "a", title: "legacy", status: "done", priority: 2, created_at: "2020-01-01T00:00:00Z", closed_at: null };
  assert.match(renderRecent("closed", [task])["#recent"].innerHTML, /closure time unknown/);
  assert.match(renderRecent("added", [task])["#recent"].innerHTML, /relative:2020-01-01T00:00:00Z/);
});

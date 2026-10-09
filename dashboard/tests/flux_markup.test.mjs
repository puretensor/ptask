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

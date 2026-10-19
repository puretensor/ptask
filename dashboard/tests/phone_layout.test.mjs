// iPhone 18 Pro Max home-screen app fit (dashboard 0.26.1), found in the iOS
// 27 simulator: 440x956 CSS px, a 62px status-bar inset. Runs the shipped
// launch-offset handler against fake scroll/touch events, and pins the CSS and
// markup contracts the phone layout depends on.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");

/* the launch-offset handler: from `let _touch` to the scroll listener's close */
const snap = shell.match(/\nlet _touch=false, _restT=0;[\s\S]*?\n\},\{passive:true\}\);\n/);
assert.ok(snap, "the launch-offset handler is in index.html");

function harness() {
  const on = {}, timers = [], scrolls = [];
  const win = {
    scrollY: 0,
    addEventListener: (type, fn) => { (on[type] ||= []).push(fn); },
    setTimeout: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    clearTimeout: (id) => { if (timers[id - 1]) timers[id - 1].fn = null; },
    scrollTo: (opts) => { scrolls.push(opts); win.scrollY = opts.top; },
  };
  new Function("win", `with (win) {${snap[0]}}`)(win);
  const fire = (type) => (on[type] || []).forEach((fn) => fn());
  const flush = () => { const due = timers.splice(0); due.forEach((t) => t.fn && t.fn()); };
  return { win, fire, flush, scrolls };
}

test("a launch parked at -62 with no finger down is put back at 0", () => {
  const h = harness();
  h.win.scrollY = -62; h.fire("scroll"); h.flush();
  assert.deepEqual(h.scrolls, [{ top: 0, behavior: "instant" }]);
});

test("a pull held under the finger is left alone", () => {
  const h = harness();
  h.fire("touchstart");
  h.win.scrollY = -80; h.fire("scroll"); h.flush();
  assert.deepEqual(h.scrolls, []);
});

test("a rubber-band bounce that settles at 0 on its own never trips it", () => {
  const h = harness();
  h.fire("touchstart"); h.fire("touchend");
  for (const y of [-90, -60, -30, -8, 0]) { h.win.scrollY = y; h.fire("scroll"); }
  h.flush();
  assert.deepEqual(h.scrolls, []);
});

test("ordinary scrolling is never touched", () => {
  const h = harness();
  for (const y of [0, 120, 447]) { h.win.scrollY = y; h.fire("scroll"); }
  h.flush();
  assert.deepEqual(h.scrolls, []);
});

test("the body grows with its content, so the tab-bar padding adds scroll room", () => {
  assert.doesNotMatch(shell, /html,body\{height:100%\}/);
  assert.match(shell, /html\{height:100%\} body\{min-height:100%\}/);
});

test("the lane picker offset follows the header's border box", () => {
  assert.match(shell, /\.observe\(document\.querySelector\('header'\),\{box:'border-box'\}\)/);
});

test("lane and review cards keep their actions outside the wrapping chip column", () => {
  const cards = shell.match(/<div class="tm"><span class="tmm">[\s\S]*?<span class="acts">/g) || [];
  assert.equal(cards.length, 2, "lane card + review card");
  for (const c of cards) assert.match(c, /<\/span>\s*<span class="acts">$/, "chips close before the actions");
  assert.match(shell, /\.tcard \.tmm\{display:contents\}/, "desktop row unchanged");
});

test("an open sheet locks the board behind it, and closing releases it", () => {
  assert.match(shell, /dr\.classList\.add\('open'\);[\s\S]{0,120}classList\.add\('sheet-open'\)/);
  assert.match(shell, /function closeDrawer\(\)\{[^}]*classList\.remove\('sheet-open'\)/);
  assert.match(shell, /html\.sheet-open,html\.sheet-open body\{overflow:hidden\}/);
});

test("the home-screen header starts below the status-bar scrim", () => {
  assert.match(shell, /@media\(display-mode:standalone\)\{header\{padding-top:calc\(env\(safe-area-inset-top,0px\) \+ 16px\)\}\}/);
});

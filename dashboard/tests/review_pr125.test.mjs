// Contract tests from the review of puretensor/ptask#125: the flux popover's
// per-actor block. Each test fails on the PR head and passes once the finding
// is fixed. Runs the shipped fluxActorsHtml() without a DOM, as
// flux_markup.test.mjs does.

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

const actor = (name, created, closed) => ({
  actor: name, created, reopened: 0, done: closed, dismissed: 0, net: created - closed,
});

// Finding 4: the list is sorted by net, largest first, and cut to six rows,
// so with more actors the biggest closers are the ones cut.
test("a cut per-actor list keeps the biggest closers", () => {
  const by_actor = [ // as /api/stats sends it: largest net first
    actor("opener-1", 9, 0), actor("opener-2", 8, 0), actor("opener-3", 7, 0),
    actor("opener-4", 6, 0), actor("opener-5", 5, 0), actor("opener-6", 4, 0),
    actor("closer-small", 0, 1), actor("closer-big", 0, 42),
  ];
  const html = load()({ by_actor });
  assert.match(html, />opener-1</, "the biggest opener is listed");
  assert.match(html, />closer-big</, "the biggest closer (net -42) is cut from the list");
});

// Finding 5: the rows are role="presentation" inside the role="menu"
// popover, and the explanation is only a hover tooltip (a title attribute).
test("per-actor rows are exposed to assistive tech", () => {
  const html = load()({ by_actor: [actor("agent-a", 2, 0), actor("agent-b", 0, 3)] });
  const rows = [...html.matchAll(/<(\w+)((?:\s[^>]*)?\sclass="[^"]*\bpma\b[^"]*"[^>]*)>/g)];
  assert.ok(rows.length >= 2, "precondition: one row per actor");
  for (const [, tag, attrs] of rows) {
    const role = (attrs.match(/\srole="([^"]*)"/) || [])[1];
    assert.doesNotMatch(attrs, /aria-hidden="true"/, `row <${tag}${attrs}> is aria-hidden`);
    assert.ok(
      role ? !["presentation", "none"].includes(role) : ["li", "tr"].includes(tag),
      `row <${tag}${attrs}> is hidden from assistive tech inside the menu (role ${role || "none"})`,
    );
  }
});

test("the per-actor explanation is not a hover-only tooltip", () => {
  const html = load()({ by_actor: [actor("agent-a", 2, 0)] });
  const bare = html.replace(/\stitle="[^"]*"/g, "");
  // Text a screen reader or a touch user gets: content, aria labels and
  // descriptions, and anything aria-describedby / aria-labelledby points to.
  const refs = [...bare.matchAll(/aria-(?:describedby|labelledby)="([^"]*)"/g)]
    .flatMap((m) => m[1].split(/\s+/));
  const referenced = refs.map((id) => {
    const el = (bare + shell).match(new RegExp(`id="${id}"[^>]*>([^<]*)`));
    return el ? el[1] : "";
  });
  const spoken = [
    bare.replace(/<[^>]*>/g, " "),
    ...[...bare.matchAll(/aria-(?:label|description)="([^"]*)"/g)].map((m) => m[1]),
    ...referenced,
  ].join(" ");
  assert.match(
    spoken,
    /dismiss/i,
    "that dismissals count as closures (unlike the chip above) is only in a title tooltip",
  );
});

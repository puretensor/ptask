// pTASK wordmark (dashboard 0.26.2): the brand reads like pCIRCUIT and pNOC,
// a white lowercase "p" then the name in capitals in the wordmark blue. Runs
// the shipped renderBrand against a fake element, and pins the markup and CSS.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");

const src = shell.match(/\nfunction renderBrand\(el,title\)\{[\s\S]*?\n\}\n/);
assert.ok(src, "renderBrand is in index.html");

function render(title) {
  const doc = { createElement: () => ({ className: "", textContent: "" }) };
  const el = {
    textContent: "",
    children: null,
    replaceChildren(...nodes) { this.children = nodes; this.textContent = ""; },
  };
  new Function("document", `${src[0]}; return renderBrand;`)(doc)(el, title);
  return el;
}

test("pTASK renders as a plain p and a blue TASK", () => {
  const el = render("pTASK");
  assert.equal(el.children.length, 2);
  assert.equal(el.children[0], "p");
  assert.equal(el.children[1].className, "wordmark-name");
  assert.equal(el.children[1].textContent, "TASK");
});

test("another tenant's brand stays plain text", () => {
  for (const title of ["ALAN", "PTASK", "pt", "pure"]) {
    const el = render(title);
    assert.equal(el.children, null, title);
    assert.equal(el.textContent, title);
  }
});

test("the shell ships the wordmark before config arrives, and defaults to pTASK", () => {
  assert.match(shell, /<b id="brandTitle">p<span class="wordmark-name">TASK<\/span><\/b>/);
  assert.match(shell, /cfg\.title:'pTASK'/);
  assert.match(shell, /<meta name="apple-mobile-web-app-title" content="pTASK">/);
  assert.match(shell, /<title>pTASK · Triage Cockpit<\/title>/);
});

test("the name is in the shared wordmark blue, the p in white", () => {
  assert.match(shell, /--wordmark:#40c4ff;/);
  assert.match(shell, /\.brandtxt b\{[^}]*color:#fff[^}]*\}/);
  assert.match(shell, /\.brandtxt b \.wordmark-name\{color:var\(--wordmark\)\}/);
});

test("the wordmark is in pCIRCUIT's system sans, not the mission theme's mono", () => {
  assert.match(shell, /--font-wordmark:-apple-system,BlinkMacSystemFont,"SF Pro Text"/);
  assert.match(shell, /\.brandtxt b\{font-family:var\(--font-wordmark\)/);
});

test("the manifest names the home-screen app pTASK", () => {
  const manifest = JSON.parse(readFileSync(new URL("../www/manifest.webmanifest", import.meta.url), "utf8"));
  assert.equal(manifest.short_name, "pTASK");
});

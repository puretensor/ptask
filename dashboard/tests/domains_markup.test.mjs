// The domain switch used to be three literal buttons (ALL/ENG/MGMT) and a pile of
// eng/mgmt literals spread through the script. A second tenant needs its own list,
// served by GET /api/config. These checks pin the seams a DOM-less test can see; the
// behavioural half lives in tests/e2e/domains-e2e.mjs.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");
const server = readFileSync(new URL("../server.py", import.meta.url), "utf8");

test("the shell asks the sidecar for its config before rendering the domain switch", () => {
  assert.match(shell, /fetch\(["']\/api\/config["']/, "index.html must GET /api/config");
  assert.match(shell, /function applyDomainConfig\(/, "a single applyDomainConfig(cfg) entry point");
});

test("the domain radiogroup is rendered from config, not hardcoded", () => {
  const group = shell.match(/<div class="domains" id="domains"[^>]*>([\s\S]*?)<\/div>/);
  assert.ok(group, "#domains radiogroup exists");
  assert.doesNotMatch(group[1], /data-d="eng"/, "no literal ENG button in the markup");
  assert.doesNotMatch(group[1], /data-d="mgmt"/, "no literal MGMT button in the markup");
});

test("keyboard order and label regex are derived, not literal", () => {
  assert.doesNotMatch(shell, /order=\['all','eng','mgmt'\]/);
  assert.doesNotMatch(shell, /\/\^domain:\(eng\|mgmt\)\$\//);
});

test("the brand title is not baked into the shell as the only source", () => {
  assert.match(shell, /id="brandTitle"/, "header brand carries an id so config can rename it");
  assert.match(shell, /id="authTitle"/, "login title keeps its id");
});

test("the sidecar exposes /api/config publicly and reads the three env knobs", () => {
  for (const knob of ["PTASK_DASH_DOMAINS", "PTASK_DASH_TITLE", "PTASK_DASH_DEFAULT_DOMAIN"]) {
    assert.ok(server.includes(knob), `${knob} must be read by server.py`);
  }
  assert.match(server, /"\/api\/config"/);
});

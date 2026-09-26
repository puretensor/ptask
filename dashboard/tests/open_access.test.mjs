// The cockpit is tailnet-gated: the HTML must ship the working board, not a login shell.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");
const server = readFileSync(new URL("../server.py", import.meta.url), "utf8");

test("the board has no login gate, password field, Face ID, or logout control", () => {
  assert.doesNotMatch(shell, /id="authGate"/);
  assert.doesNotMatch(shell, /id="authPassword"/);
  assert.doesNotMatch(shell, /id="authForm"/);
  assert.doesNotMatch(shell, /id="authTitle"/);
  assert.doesNotMatch(shell, /face-unlock/);
  assert.doesNotMatch(shell, /\/api\/auth\//);
  assert.doesNotMatch(shell, /id="logout-btn"/);
  assert.doesNotMatch(shell, /type="password"/);
  assert.match(shell, /function startApp\(/);
  assert.match(shell, /_configReady\.then\(\(\)=>startApp\(\)\)/);
});

test("the sidecar has no human-auth machinery and keeps PTASK_ACTOR", () => {
  assert.doesNotMatch(server, /PTASK_DASH_PASS/);
  assert.doesNotMatch(server, /PTASK_DASH_USER/);
  assert.doesNotMatch(server, /WWW-Authenticate/);
  assert.doesNotMatch(server, /session_auth/);
  assert.doesNotMatch(server, /\/api\/auth\/login/);
  assert.match(server, /PTASK_ACTOR/);
  assert.match(server, /path in \("\/login", "\/logout"\)/);
});

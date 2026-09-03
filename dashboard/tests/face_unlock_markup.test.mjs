// The boot module reaches into the login shell by id, and nothing else would notice if one of
// those ids were renamed: the Face ID rows would simply never appear, and the gate would keep
// working, so the loss would be silent. This test makes the coupling explicit and checkable.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const boot = readFileSync(new URL("../www/face-unlock-boot.js", import.meta.url), "utf8");
const shell = readFileSync(new URL("../www/index.html", import.meta.url), "utf8");
const server = readFileSync(new URL("../server.py", import.meta.url), "utf8");

test("every id the boot module looks up exists in the login shell", () => {
  const ids = [...boot.matchAll(/\$\("([^"]+)"\)/g)].map((m) => m[1]);
  assert.ok(ids.length >= 4, `expected the boot module to address the shell by id, found ${ids.length}`);
  for (const id of new Set(ids)) {
    assert.match(shell, new RegExp(`id="${id}"`), `#${id} is queried by face-unlock-boot.js but absent from index.html`);
  }
});

test("the login form hands the typed password to the boot module before reloading", () => {
  assert.match(
    shell,
    /afterPasswordLogin\(password\)/,
    "the sign-in handler must offer enrolment while the password is still in hand",
  );
  assert.match(boot, /afterPasswordLogin/, "the boot module must expose the hook the shell calls");
});

test("both Face ID modules are reachable from the unauthenticated login shell", () => {
  for (const asset of ["/face-unlock.js", "/face-unlock-boot.js"]) {
    assert.ok(
      server.includes(`"${asset}"`),
      `${asset} is loaded by the public login shell, so it must be in PUBLIC_ASSETS`,
    );
  }
  assert.match(shell, /<script type="module" src="face-unlock-boot\.js"><\/script>/);
});

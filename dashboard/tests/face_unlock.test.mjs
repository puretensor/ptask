// Contract tests for the shared face-unlock core. Byte-identical across pNOC, pTask and pScope —
// the module is vendored, so the tests are vendored with it and a fix must pass in all three.
//
// No browser here: the PRF provider and the record store are both injected, exactly as the
// production wiring injects the real WebAuthn provider and IndexedDB. What is under test is the
// wrap, the three refusals, and the sign-in contract — not the platform's passkey sheet.

import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

import {
  FaceUnlockError,
  RECORD_VERSION,
  createFaceUnlock,
  faceUnlockMessage,
  memoryStore,
  signInWithFace,
} from "../www/face-unlock.js";

// The crypto core is VENDORED byte-identical into pNOC, pTask and pScope. This pin is what makes
// that claim checkable rather than aspirational: edit face-unlock.js and the test goes red in
// every repo until the copy AND this digest are updated in all three together.
const CORE_SHA256 = "f155b4c72dbd290e1dea86c7925268a2b5e52d02bf84202ca0484a889fcec429";

const APP = { appId: "testapp", rpId: "test.puretensor.ai", origin: "https://test.puretensor.ai" };
const PASSWORD = "correct horse battery staple";

/** A deterministic stand-in for one platform authenticator. */
function fakePrf(seed = "authenticator-a") {
  const secretFor = (credentialId) => {
    const bytes = new Uint8Array(32);
    const material = `${seed}:${credentialId}`;
    for (let i = 0; i < bytes.length; i += 1) bytes[i] = material.charCodeAt(i % material.length) ^ (i * 31);
    return bytes;
  };
  return {
    registered: 0,
    asserted: 0,
    async isSupported() {
      return true;
    },
    async register() {
      this.registered += 1;
      const credentialId = `cred-${this.registered}`;
      return { credentialId, secret: secretFor(credentialId) };
    },
    async evaluate({ credentialId }) {
      this.asserted += 1;
      return { secret: secretFor(credentialId) };
    },
  };
}

const build = (overrides = {}) => {
  const prf = overrides.prf ?? fakePrf();
  const store = overrides.store ?? memoryStore();
  return { prf, store, faceUnlock: createFaceUnlock({ prf, store, ...APP, ...overrides.app }) };
};

const codeOf = async (fn) => {
  try {
    await fn();
  } catch (error) {
    assert.ok(error instanceof FaceUnlockError, `expected FaceUnlockError, got ${error}`);
    return error.code;
  }
  return assert.fail("expected a refusal, got a value");
};

test("enrol then reveal round-trips the dashboard password", async () => {
  const { faceUnlock, prf } = build();
  assert.equal(await faceUnlock.isEnrolled(), false);
  await faceUnlock.enroll(PASSWORD);
  assert.equal(await faceUnlock.isEnrolled(), true);
  assert.equal(await faceUnlock.reveal(), PASSWORD);
  assert.equal(prf.registered, 1);
});

test("the stored record never contains the password in the clear", async () => {
  const { faceUnlock, store } = build();
  await faceUnlock.enroll(PASSWORD);
  const record = await store.load();
  assert.equal(record.version, RECORD_VERSION);
  assert.ok(!JSON.stringify(record).includes(PASSWORD));
  assert.ok(!Buffer.from(record.wrapped, "base64").toString("utf8").includes(PASSWORD));
});

test("enrol refuses an empty password", async () => {
  const { faceUnlock } = build();
  assert.equal(await codeOf(() => faceUnlock.enroll("")), "enroll");
  assert.equal(await codeOf(() => faceUnlock.enroll(undefined)), "enroll");
});

test("forget drops the enrolment", async () => {
  const { faceUnlock } = build();
  await faceUnlock.enroll(PASSWORD);
  await faceUnlock.forget();
  assert.equal(await faceUnlock.isEnrolled(), false);
  assert.equal(await codeOf(() => faceUnlock.reveal()), "not-enrolled");
});

test("REFUSAL binding: a record enrolled under another hostname is refused, not retried", async () => {
  const { store, prf } = build();
  const old = createFaceUnlock({ prf, store, ...APP, rpId: "forzieri.puretensor.ai", origin: "https://forzieri.puretensor.ai" });
  await old.enroll(PASSWORD);
  const moved = createFaceUnlock({ prf, store, ...APP });
  assert.equal(await moved.isEnrolled(), false);
  assert.equal(await codeOf(() => moved.reveal()), "stale-binding");
  assert.equal(prf.asserted, 0, "a stale binding must never reach the authenticator");
});

test("REFUSAL binding: same host, different origin (scheme or port) is refused", async () => {
  const { store, prf } = build();
  const insecure = createFaceUnlock({ prf, store, ...APP, origin: "http://test.puretensor.ai:8080" });
  await insecure.enroll(PASSWORD);
  assert.equal(await codeOf(() => createFaceUnlock({ prf, store, ...APP }).reveal()), "stale-binding");
});

test("REFUSAL binding: a record written by another app is refused", async () => {
  const { store, prf } = build();
  await createFaceUnlock({ prf, store, ...APP, appId: "otherapp" }).enroll(PASSWORD);
  assert.equal(await codeOf(() => createFaceUnlock({ prf, store, ...APP }).reveal()), "stale-binding");
});

test("REFUSAL version: an unknown record version is refused", async () => {
  const { faceUnlock, store } = build();
  await faceUnlock.enroll(PASSWORD);
  await store.save({ ...(await store.load()), version: RECORD_VERSION + 1 });
  assert.equal(await faceUnlock.isEnrolled(), false);
  assert.equal(await codeOf(() => faceUnlock.reveal()), "version");
});

test("REFUSAL integrity: a tampered wrap fails to open rather than yielding a wrong password", async () => {
  const { faceUnlock, store } = build();
  await faceUnlock.enroll(PASSWORD);
  const record = await store.load();
  const bytes = Buffer.from(record.wrapped, "base64");
  bytes[0] ^= 0xff;
  await store.save({ ...record, wrapped: bytes.toString("base64") });
  assert.equal(await codeOf(() => faceUnlock.reveal()), "decrypt-failed");
});

test("REFUSAL integrity: another authenticator's PRF secret cannot open the wrap", async () => {
  const store = memoryStore();
  await createFaceUnlock({ prf: fakePrf("authenticator-a"), store, ...APP }).enroll(PASSWORD);
  const stolen = createFaceUnlock({ prf: fakePrf("authenticator-b"), store, ...APP });
  assert.equal(await codeOf(() => stolen.reveal()), "decrypt-failed");
});

test("a cancelled or timed-out assertion surfaces as prf-failed, keeping the enrolment", async () => {
  const prf = fakePrf();
  prf.evaluate = async () => {
    throw new Error("NotAllowedError");
  };
  const { faceUnlock } = build({ prf });
  await faceUnlock.enroll(PASSWORD);
  assert.equal(await codeOf(() => faceUnlock.reveal()), "prf-failed");
  assert.equal(await faceUnlock.isEnrolled(), true, "a cancelled Face ID must not drop the enrolment");
});

test("REFUSAL currency: a 401 from the sign-in route drops the enrolment", async () => {
  const { faceUnlock } = build();
  await faceUnlock.enroll(PASSWORD);
  const seen = [];
  const result = await signInWithFace({
    faceUnlock,
    signIn: async (password) => {
      seen.push(password);
      return 401;
    },
  });
  assert.deepEqual(result, { ok: false, status: 401, forgotten: true });
  assert.deepEqual(seen, [PASSWORD]);
  assert.equal(await faceUnlock.isEnrolled(), false);
});

test("signInWithFace keeps the enrolment on success and on a recoverable failure", async () => {
  for (const [status, ok] of [
    [200, true],
    [204, true],
    [429, false],
    [503, false],
    [0, false],
  ]) {
    const { faceUnlock } = build();
    await faceUnlock.enroll(PASSWORD);
    const result = await signInWithFace({ faceUnlock, signIn: async () => status });
    assert.deepEqual(result, { ok, status, forgotten: false }, `status ${status}`);
    assert.equal(await faceUnlock.isEnrolled(), true, `status ${status} must keep the enrolment`);
  }
});

test("re-enrolling replaces the record instead of stacking a second one", async () => {
  const { faceUnlock, store, prf } = build();
  await faceUnlock.enroll(PASSWORD);
  await faceUnlock.enroll("a rotated password");
  assert.equal(prf.registered, 2);
  assert.equal((await store.load()).credentialId, "cred-2");
  assert.equal(await faceUnlock.reveal(), "a rotated password");
});

test("createFaceUnlock refuses incomplete configuration", async () => {
  const store = memoryStore();
  const prf = fakePrf();
  assert.equal(await codeOf(async () => createFaceUnlock({ prf, store, ...APP, rpId: "" })), "config");
  assert.equal(await codeOf(async () => createFaceUnlock({ prf: null, store, ...APP })), "config");
});

test("every refusal has operator-facing copy, and none of it leaks the password", async () => {
  for (const code of ["not-enrolled", "stale-binding", "version", "decrypt-failed", "prf-unsupported", "prf-failed", "storage"]) {
    const message = faceUnlockMessage(new FaceUnlockError(code));
    assert.notEqual(message, code, `no operator-facing copy for ${code}`);
    assert.match(message, /^[A-Z].*\.$/, `not a sentence for ${code}: ${message}`);
  }
  assert.equal(faceUnlockMessage(new Error("boom")), "boom");
  assert.equal(faceUnlockMessage(null), "Face ID unlock failed.");
});

test("the vendored crypto core is byte-identical across pNOC, pTask and pScope", () => {
  const source = readFileSync(new URL("../www/face-unlock.js", import.meta.url));
  assert.equal(
    createHash("sha256").update(source).digest("hex"),
    CORE_SHA256,
    "face-unlock.js drifted from the other two repos — re-vendor it and update CORE_SHA256 in all three",
  );
});

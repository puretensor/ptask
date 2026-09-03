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
  RECOVERABLE_CODES,
  createFaceUnlock,
  faceUnlockMessage,
  isRecoverable,
  memoryStore,
  signInWithFace,
  webauthnPrf,
} from "../www/face-unlock.js";

// The crypto core is VENDORED byte-identical into pNOC, pTask and pScope, and this pin records the
// digest all three must carry. BE PRECISE ABOUT WHAT IT CATCHES: each repo compares its OWN copy to
// its OWN literal, so it catches an edit made without updating the pin — it does NOT, on its own,
// prove the three repos agree. Updating one repo's copy and its pin together leaves a green board
// and three divergent files. The cross-repo comparison is `scripts/verify-vendored-core.sh` in
// pTask, which reads all three paths; run it when you touch this file.
const CORE_SHA256 = "d5d844dfcd0db006de2079af51844399c68128a4811d7f176d39924eb306ac66";

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

/**
 * Run `fn` with navigator.credentials stubbed. globalThis.navigator is an accessor in modern Node,
 * so it can only be replaced through defineProperty — plain assignment throws.
 */
async function withCredentials(credentials, fn) {
  const had = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const hadPkc = globalThis.PublicKeyCredential;
  Object.defineProperty(globalThis, "navigator", { value: { credentials }, configurable: true, writable: true });
  globalThis.PublicKeyCredential = function PublicKeyCredential() {};
  try {
    return await fn();
  } finally {
    if (had) Object.defineProperty(globalThis, "navigator", had);
    else delete globalThis.navigator;
    if (hadPkc === undefined) delete globalThis.PublicKeyCredential;
    else globalThis.PublicKeyCredential = hadPkc;
  }
}

/** A credential whose PRF result is exactly `bytes`. */
const prfCredential = (bytes) => ({
  rawId: new Uint8Array(16).buffer,
  getClientExtensionResults: () => ({ prf: { results: { first: bytes.buffer } } }),
});


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

// NOTE ON THE THREE BINDING TESTS BELOW: they share ONE memoryStore between two differently
// configured instances. Real IndexedDB is partitioned by origin, so that state is unconstructible
// in a browser from an ordinary hostname change — there you get "not-enrolled" and a fresh
// enrolment, not "stale-binding". These tests pin the check for the cases where the store IS
// shared: a restored backup, a migrated database, and a future RECORD_VERSION bump.
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
    "face-unlock.js changed without its pin. Re-vendor it to ALL THREE repos, update CORE_SHA256 in all three, and run pTask scripts/verify-vendored-core.sh to prove they agree.",
  );
});

// ---------------------------------------------------------------- regressions from the 2026-09-03 adversarial review

test("REGRESSION: a degenerate PRF secret is refused, not used as a key", async () => {
  // An authenticator returning a constant (all-zero is the observed quirk) would make the wrapping
  // key derivable by anyone, and two such devices would open each other's wrap. A short secret is
  // simply a broken authenticator. Both must refuse rather than silently produce a weak key.
  for (const [label, bytes] of [
    ["all-zero", new Uint8Array(32)],
    ["all-0xFF", new Uint8Array(32).fill(0xff)],
    ["one byte", new Uint8Array(1)],
    ["empty", new Uint8Array(0)],
    ["31 bytes", Uint8Array.from({ length: 31 }, (_, i) => i + 1)],
  ]) {
    const cred = prfCredential(bytes);
    const code = await withCredentials({ get: async () => cred, create: async () => cred }, async () => {
      const provider = webauthnPrf({ rpId: "test.puretensor.ai", appId: "testapp", appName: "Test" });
      try {
        await provider.evaluate({ credentialId: "AAAAAAAAAAAAAAAAAAAAAA" });
      } catch (error) {
        assert.ok(error instanceof FaceUnlockError, `${label}: expected a refusal, got ${error}`);
        return error.code;
      }
      return null;
    });
    assert.equal(code, "prf-unsupported", `${label}: a degenerate PRF secret must be refused`);
  }
});

test("a well-formed 32-byte PRF secret is accepted", async () => {
  const cred = prfCredential(Uint8Array.from({ length: 32 }, (_, i) => i + 1));
  const secret = await withCredentials({ get: async () => cred }, async () => {
    const provider = webauthnPrf({ rpId: "test.puretensor.ai", appId: "testapp", appName: "Test" });
    return (await provider.evaluate({ credentialId: "AAAAAAAAAAAAAAAAAAAAAA" })).secret;
  });
  assert.equal(secret.byteLength, 32);
});

test("REGRESSION: the recovery code list is shared, so the three wirings cannot drift apart", () => {
  // pSCOPE once omitted decrypt-failed from its own hand-written copy of this list, which left a
  // corrupt record showing a Face ID button that could never succeed and never be re-enrolled.
  assert.deepEqual([...RECOVERABLE_CODES].sort(), ["decrypt-failed", "not-enrolled", "stale-binding", "version"]);
  for (const code of RECOVERABLE_CODES) {
    assert.ok(isRecoverable(new FaceUnlockError(code)), `${code} must be recoverable`);
  }
  // A cancelled prompt must NOT drop a good enrolment, and an unsupported device must not either.
  for (const code of ["prf-failed", "prf-unsupported", "storage", "config", "enroll"]) {
    assert.equal(isRecoverable(new FaceUnlockError(code)), false, `${code} must NOT be recoverable`);
  }
  assert.equal(isRecoverable(new Error("decrypt-failed")), false, "a plain Error must never count");
  assert.equal(isRecoverable(null), false);
});

test("REGRESSION: the rpId binding is enforced on its own, not merely implied by the origin", async () => {
  // The original suite only ever changed rpId and origin together, so deleting the rpId check
  // entirely left every test green. Change ONLY rpId: the record must still be refused.
  const { store, prf } = build();
  await createFaceUnlock({ prf, store, ...APP, rpId: "other.puretensor.ai" }).enroll(PASSWORD);
  assert.equal(await codeOf(() => createFaceUnlock({ prf, store, ...APP }).reveal()), "stale-binding");
  assert.equal(prf.asserted, 0, "a stale binding must never reach the authenticator");
});

test("REGRESSION: user verification is required, and deleting that would fail this test", async () => {
  // "Face ID" IS the user-verification requirement. Removing it left every test green, including
  // the real-Chromium e2e, so the feature's entire premise was unverified. Assert the ceremony
  // options the provider actually asks for.
  const asked = [];
  const cred = prfCredential(Uint8Array.from({ length: 32 }, (_, i) => i + 1));
  await withCredentials(
    {
      create: async (o) => { asked.push(["create", o.publicKey]); return cred; },
      get: async (o) => { asked.push(["get", o.publicKey]); return cred; },
    },
    async () => webauthnPrf({ rpId: "test.puretensor.ai", appId: "testapp", appName: "Test" }).register(),
  );
  const create = asked.find(([kind]) => kind === "create")[1];
  const get = asked.find(([kind]) => kind === "get")[1];
  assert.equal(create.authenticatorSelection.userVerification, "required", "creation must require user verification");
  assert.equal(create.authenticatorSelection.residentKey, "required");
  assert.equal(create.authenticatorSelection.authenticatorAttachment, "platform");
  assert.equal(get.userVerification, "required", "assertion must require user verification");
  assert.equal(get.rpId, "test.puretensor.ai", "rpId must be the serving hostname, never an apex");
  assert.ok(create.extensions.prf, "the prf extension must be requested at creation");
  assert.ok(get.extensions.prf.eval.first, "the assertion must evaluate the PRF salt");
});

test("REGRESSION: re-enrolling excludes the credential already on this device", async () => {
  // Without excludeCredentials the platform mints a SECOND resident passkey for the same site on
  // every enrolment, and nothing in the app can delete the orphan — the operator's Settings list
  // fills with identical entries, exactly one of which works.
  const store = memoryStore();
  const seen = [];
  const cred = prfCredential(Uint8Array.from({ length: 32 }, (_, i) => i + 1));
  const provider = {
    async isSupported() { return true; },
    async register(exclude = []) { seen.push(exclude); return { credentialId: `cred-${seen.length}`, secret: Uint8Array.from({ length: 32 }, (_, i) => i + seen.length) }; },
    async evaluate({ credentialId }) { return { secret: Uint8Array.from({ length: 32 }, (_, i) => i + Number(credentialId.split("-")[1])) }; },
  };
  const fu = createFaceUnlock({ prf: provider, store, ...APP });
  await fu.enroll(PASSWORD);
  assert.deepEqual(seen[0], [], "the first enrolment has nothing to exclude");
  await fu.enroll(PASSWORD);
  assert.deepEqual(seen[1], ["cred-1"], "a re-enrolment must exclude the credential already held");
  void cred;
});

test("the real provider passes excludeCredentials through to the platform", async () => {
  const asked = [];
  const cred = prfCredential(Uint8Array.from({ length: 32 }, (_, i) => i + 1));
  await withCredentials(
    {
      create: async (o) => { asked.push(o.publicKey); return cred; },
      get: async () => cred,
    },
    async () => webauthnPrf({ rpId: "test.puretensor.ai", appId: "testapp", appName: "Test" }).register(["AAAAAAAAAAAAAAAAAAAAAA"]),
  );
  assert.equal(asked[0].excludeCredentials.length, 1, "the excluded credential must reach the ceremony");
  assert.equal(asked[0].excludeCredentials[0].type, "public-key");
  assert.ok(asked[0].excludeCredentials[0].id instanceof Uint8Array);
});

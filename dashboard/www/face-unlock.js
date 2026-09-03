"use strict";
// Face unlock for a PureTensor dashboard: a WebAuthn PRF secret wraps this dashboard's
// sign-in password in IndexedDB, so the operator taps Face ID instead of typing it.
//
// Replicated from pureKEY (forzieri) `www/core/unlock.js` — same crypto, same refusals, a
// different plaintext. There the wrapped secret is a Bitwarden user key; here it is the shared
// dashboard password, which the caller then POSTs to the dashboard's existing sign-in endpoint.
// That substitution is the whole reason this needs NO server change: the server still only ever
// sees a password, over the login route it already has, with the throttle it already applies.
//
// The PRF output is a deterministic 32 bytes bound to one platform credential, released only
// after user verification (Face ID / Touch ID / device passcode). HKDF turns it into a
// NON-EXTRACTABLE AES-GCM key that wraps the password; the wrapped blob is useless without the
// authenticator. Nothing here is novel cryptography — WebCrypto HKDF + AES-GCM around a string.
//
// Three refusals gate a reveal, and each is a refusal, never a repair. The password is the only
// fallback:
//   * binding    — the record carries the rpId and origin it was enrolled under. A hostname
//                  cutover changes which passkey the platform will even offer (pureKEY had to
//                  re-enrol by hand when forzieri.puretensor.ai became key.puretensor.ai), so a
//                  record from the old name is dropped, never retried.
//   * integrity  — AES-GCM authenticates the wrap: a tampered, truncated or foreign record
//                  fails to open rather than yielding a plausible wrong password.
//   * currency   — the server is the canary. `reveal()` hands back a password; if the sign-in
//                  route answers 401 the dashboard password was rotated since enrolment, and the
//                  caller must `forget()` instead of re-prompting for a Face ID that cannot sign
//                  in. `signInWithFace()` below encodes that contract so no caller can forget it.
//
// Storage is IndexedDB only. Never localStorage: it is readable by any script on the origin and
// survives in places the operator does not expect a credential to survive.

export const RECORD_VERSION = 1;

/** Error carrying a stable machine-readable `code` alongside the human message. */
export class FaceUnlockError extends Error {
  /**
   * @param {string} code
   * @param {string} [message]
   */
  constructor(code, message) {
    super(message || code);
    this.name = "FaceUnlockError";
    this.code = code;
  }
}

const enc = new TextEncoder();
const dec = new TextDecoder();

/** @param {ArrayBuffer | Uint8Array} bytes */
const b64 = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes)));
/** @param {string} text */
const unb64 = (text) => Uint8Array.from(atob(text), (ch) => ch.charCodeAt(0));
/** @param {ArrayBuffer | Uint8Array} bytes */
const b64url = (bytes) => b64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
/** @param {string} text */
const unb64url = (text) => unb64(text.replace(/-/g, "+").replace(/_/g, "/"));

/**
 * A fresh ArrayBuffer-backed copy of any BufferSource. WebAuthn hands back a `BufferSource` and
 * WebCrypto wants a concrete view; copying once here keeps every downstream `fill(0)` meaningful
 * and keeps the module type-checkable where it is consumed from TypeScript (pNOC).
 * @param {BufferSource} source
 * @returns {Uint8Array<ArrayBuffer>}
 */
function bytesOf(source) {
  const view = ArrayBuffer.isView(source)
    ? new Uint8Array(source.buffer, source.byteOffset, source.byteLength)
    : new Uint8Array(source);
  const copy = new Uint8Array(view.byteLength);
  copy.set(view);
  return copy;
}

/**
 * Message text from an unknown thrown value, without assuming it is an Error.
 * @param {unknown} error
 * @returns {string}
 */
function messageOf(error) {
  if (error instanceof Error && error.message) return error.message;
  return typeof error === "string" && error ? error : "";
}

// ---------------------------------------------------------------- PRF provider (real WebAuthn)

/**
 * @typedef {object} PrfProvider
 * @property {() => Promise<boolean>} isSupported
 * @property {() => Promise<{ credentialId: string, secret: Uint8Array<ArrayBuffer> }>} register
 * @property {(args: { credentialId: string }) => Promise<{ secret: Uint8Array<ArrayBuffer> }>} evaluate
 */

/**
 * Platform-authenticator PRF provider. `rpId` must be exactly the serving hostname — never the
 * apex, never a parent domain the browser would also accept, because the rpId is what the
 * platform keys the passkey by system-wide.
 *
 * @param {object} options
 * @param {string} options.rpId          serving hostname, e.g. "noc.puretensor.ai"
 * @param {string} options.appId         short slug scoping the PRF salt and HKDF info, e.g. "purenoc"
 * @param {string} options.appName       display name shown in the platform passkey sheet
 * @param {string} [options.userName]
 * @param {string} [options.displayName]
 * @param {number} [options.timeoutMs]
 * @returns {PrfProvider}
 */
export function webauthnPrf({ rpId, appId, appName, userName = "operator", displayName = "", timeoutMs = 60_000 }) {
  if (!rpId) throw new FaceUnlockError("config", "webauthnPrf needs the serving hostname as rpId");
  if (!appId) throw new FaceUnlockError("config", "webauthnPrf needs an appId");
  const salt = enc.encode(`${appId}.prf.v1`);

  /** @param {PublicKeyCredential | null} result */
  const prfOf = (result) => {
    const first = result?.getClientExtensionResults?.()?.prf?.results?.first;
    if (!first) throw new FaceUnlockError("prf-unsupported", "this authenticator did not return a PRF secret");
    return bytesOf(first);
  };

  // An unanswered platform prompt otherwise hangs forever — always give it an abort signal.
  /**
   * @template T
   * @param {(signal: AbortSignal) => Promise<T>} run
   * @returns {Promise<T>}
   */
  const withTimeout = (run) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    return run(controller.signal).finally(() => clearTimeout(timer));
  };

  /** @param {PublicKeyCredentialDescriptor[]} allowCredentials */
  const assert = async (allowCredentials) =>
    /** @type {PublicKeyCredential | null} */ (
      await withTimeout((signal) =>
        navigator.credentials.get({
          signal,
          publicKey: {
            challenge: crypto.getRandomValues(new Uint8Array(32)),
            rpId,
            userVerification: "required",
            timeout: timeoutMs,
            allowCredentials,
            extensions: { prf: { eval: { first: salt } } },
          },
        }),
      )
    );

  return {
    async isSupported() {
      if (!globalThis.PublicKeyCredential) return false;
      if (!globalThis.isSecureContext) return false;
      const available = await PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable?.().catch(() => false);
      return Boolean(available);
    },

    async register() {
      const cred = /** @type {PublicKeyCredential | null} */ (
        await withTimeout((signal) =>
          navigator.credentials.create({
            signal,
            publicKey: {
              challenge: crypto.getRandomValues(new Uint8Array(32)),
              rp: { name: appName || appId, id: rpId },
              user: { id: crypto.getRandomValues(new Uint8Array(16)), name: userName, displayName: displayName || userName },
              pubKeyCredParams: [
                { type: "public-key", alg: -7 },
                { type: "public-key", alg: -257 },
              ],
              authenticatorSelection: {
                authenticatorAttachment: "platform",
                residentKey: "required",
                userVerification: "required",
              },
              timeout: timeoutMs,
              extensions: { prf: {} },
            },
          }),
        )
      );
      if (!cred) throw new FaceUnlockError("prf-failed", "passkey creation was cancelled");
      // The PRF output is only released on an assertion, never at creation.
      const secret = prfOf(await assert([{ type: "public-key", id: cred.rawId }]));
      return { credentialId: b64url(cred.rawId), secret };
    },

    async evaluate({ credentialId }) {
      const result = await assert([{ type: "public-key", id: unb64url(credentialId) }]);
      if (!result) throw new FaceUnlockError("prf-failed", "unlock was cancelled");
      return { secret: prfOf(result) };
    },
  };
}

// ---------------------------------------------------------------- store (IndexedDB only)

/**
 * @typedef {object} EnrolmentRecord
 * @property {number} version
 * @property {string} appId
 * @property {string} rpId
 * @property {string} origin
 * @property {string} credentialId
 * @property {string} iv
 * @property {string} wrapped
 * @property {string} enrolledAt
 */

/**
 * @typedef {object} RecordStore
 * @property {() => Promise<EnrolmentRecord | null>} load
 * @property {(record: EnrolmentRecord) => Promise<unknown>} save
 * @property {() => Promise<unknown>} clear
 */

/**
 * @param {{ dbName?: string, storeName?: string, key?: string }} [options]
 * @returns {RecordStore}
 */
export function indexedDbStore({ dbName = "face-unlock", storeName = "enrolment", key = "record" } = {}) {
  const open = () =>
    new Promise(/** @param {(db: IDBDatabase) => void} resolve */ (resolve, reject) => {
      const request = indexedDB.open(dbName, 1);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(storeName)) db.createObjectStore(storeName);
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error || new FaceUnlockError("storage", "IndexedDB unavailable"));
    });

  /**
   * @template T
   * @param {IDBTransactionMode} mode
   * @param {(store: IDBObjectStore) => IDBRequest<T> | undefined} run
   * @returns {Promise<T | null>}
   */
  const tx = async (mode, run) => {
    const db = await open();
    try {
      return await new Promise((resolve, reject) => {
        const transaction = db.transaction(storeName, mode);
        const result = run(transaction.objectStore(storeName));
        transaction.oncomplete = () => resolve(result ? result.result : null);
        transaction.onabort = transaction.onerror = () =>
          reject(transaction.error || new FaceUnlockError("storage", "IndexedDB failed"));
      });
    } finally {
      db.close();
    }
  };

  return {
    load: () => tx("readonly", (store) => store.get(key)),
    save: (record) => tx("readwrite", (store) => store.put(record, key)),
    clear: () => tx("readwrite", (store) => store.delete(key)),
  };
}

/**
 * In-memory store. Tests only — it does not survive a reload, which is the point.
 * @returns {RecordStore}
 */
export function memoryStore() {
  /** @type {EnrolmentRecord | null} */
  let held = null;
  return {
    load: async () => held,
    save: async (record) => {
      held = record;
    },
    clear: async () => {
      held = null;
    },
  };
}

// ---------------------------------------------------------------- the unlock itself

/**
 * @param {object} options
 * @param {PrfProvider} options.prf
 * @param {RecordStore} options.store
 * @param {string} options.appId    scopes the HKDF info; must match the provider's appId
 * @param {string} options.rpId     the hostname this enrolment is bound to
 * @param {string} options.origin   the origin this enrolment is bound to
 * @param {SubtleCrypto} [options.subtle]
 */
export function createFaceUnlock({ prf, store, appId, rpId, origin, subtle = globalThis.crypto?.subtle }) {
  if (!prf || !store) throw new FaceUnlockError("config", "createFaceUnlock needs a prf provider and a store");
  if (!appId || !rpId || !origin) throw new FaceUnlockError("config", "createFaceUnlock needs appId, rpId and origin");
  if (!subtle) throw new FaceUnlockError("config", "WebCrypto SubtleCrypto is unavailable");

  const hkdfSalt = enc.encode(`${appId}.prf.v1`);
  const hkdfInfo = enc.encode(`${appId}.unlock.v1`);

  /** @param {Uint8Array<ArrayBuffer>} secret */
  const wrappingKeyFrom = async (secret) => {
    const base = await subtle.importKey("raw", secret, "HKDF", false, ["deriveKey"]);
    return subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt: hkdfSalt, info: hkdfInfo },
      base,
      { name: "AES-GCM", length: 256 },
      false, // non-extractable: the wrapping key never becomes bytes JS can read
      ["encrypt", "decrypt"],
    );
  };

  /**
   * The binding refusals. A record that does not belong to this app, version, hostname or origin
   * is dropped — asserting against it would prompt for a passkey the platform will not offer.
   * @returns {Promise<EnrolmentRecord>}
   */
  const readRecord = async () => {
    const record = await store.load();
    if (!record) throw new FaceUnlockError("not-enrolled", "this device is not enrolled");
    if (record.version !== RECORD_VERSION)
      throw new FaceUnlockError("version", `unknown enrolment record version ${record.version}`);
    if (record.appId !== appId) throw new FaceUnlockError("stale-binding", "this enrolment belongs to another app");
    if (record.rpId !== rpId)
      throw new FaceUnlockError("stale-binding", "this device was enrolled under a different hostname");
    if (record.origin !== origin)
      throw new FaceUnlockError("stale-binding", "this device was enrolled under a different origin");
    return record;
  };

  return {
    async isSupported() {
      return prf.isSupported ? prf.isSupported() : true;
    },

    /** True only for a record this build can actually use — a stale binding reads as not enrolled. */
    async isEnrolled() {
      try {
        await readRecord();
        return true;
      } catch {
        return false;
      }
    },

    /**
     * Bind a new platform passkey to this device and wrap `password` under its PRF secret.
     * Replaces any existing record: one device, one enrolment.
     * @param {string} password
     */
    async enroll(password) {
      if (typeof password !== "string" || password === "")
        throw new FaceUnlockError("enroll", "a dashboard password is required");
      const { credentialId, secret } = await prf.register();
      const wrappingKey = await wrappingKeyFrom(secret);
      secret.fill(0);
      const iv = crypto.getRandomValues(new Uint8Array(12));
      const payload = enc.encode(password);
      const wrapped = await subtle.encrypt({ name: "AES-GCM", iv }, wrappingKey, payload);
      payload.fill(0);
      await store.save({
        version: RECORD_VERSION,
        appId,
        rpId,
        origin,
        credentialId,
        iv: b64(iv),
        wrapped: b64(wrapped),
        enrolledAt: new Date().toISOString(),
      });
    },

    /**
     * Face ID -> the dashboard password. The caller MUST treat a 401 from the sign-in route as a
     * rotated password and call `forget()`; prefer `signInWithFace()`, which does that for you.
     * @returns {Promise<string>}
     */
    async reveal() {
      const record = await readRecord();
      let secret;
      try {
        ({ secret } = await prf.evaluate({ credentialId: record.credentialId }));
      } catch (error) {
        throw error instanceof FaceUnlockError ? error : new FaceUnlockError("prf-failed", messageOf(error));
      }
      const wrappingKey = await wrappingKeyFrom(secret);
      secret.fill(0);
      let plain;
      try {
        plain = await subtle.decrypt({ name: "AES-GCM", iv: unb64(record.iv) }, wrappingKey, unb64(record.wrapped));
      } catch {
        throw new FaceUnlockError("decrypt-failed", "the stored password could not be opened by this authenticator");
      }
      const password = dec.decode(plain);
      new Uint8Array(plain).fill(0);
      return password;
    },

    async forget() {
      await store.clear();
    },
  };
}

/**
 * @typedef {ReturnType<typeof createFaceUnlock>} FaceUnlock
 */

/**
 * The currency refusal, in code so no call site can skip it: reveal, sign in, and drop the
 * enrolment the moment the server says the password no longer works.
 *
 * `signIn` must resolve to the sign-in response status: 2xx = signed in, 401 = the dashboard
 * password was rotated (forget), anything else (429 lockout, 5xx, offline) = keep the enrolment
 * and let the operator retry.
 *
 * @param {object} options
 * @param {FaceUnlock} options.faceUnlock
 * @param {(password: string) => Promise<number>} options.signIn
 * @returns {Promise<{ ok: boolean, status: number, forgotten: boolean }>}
 */
export async function signInWithFace({ faceUnlock, signIn }) {
  const password = await faceUnlock.reveal();
  const status = await signIn(password);
  if (status === 401) {
    await faceUnlock.forget();
    return { ok: false, status, forgotten: true };
  }
  return { ok: status >= 200 && status < 300, status, forgotten: false };
}

/**
 * Human-readable copy for a failed unlock. Kept here so all three dashboards say the same thing.
 * @param {unknown} error
 * @returns {string}
 */
export function faceUnlockMessage(error) {
  const code = error instanceof FaceUnlockError ? error.code : "";
  switch (code) {
    case "not-enrolled":
      return "Face ID is not set up on this device.";
    case "stale-binding":
    case "version":
      return "Face ID needs setting up again on this device. Sign in with the password.";
    case "decrypt-failed":
      return "Face ID could not open the saved password. Sign in with the password.";
    case "prf-unsupported":
      return "This device's Face ID cannot hold a password. Sign in with the password.";
    case "prf-failed":
      return "Face ID was cancelled or timed out.";
    case "storage":
      return "This browser blocked on-device storage, so Face ID is unavailable.";
    default:
      return messageOf(error) || "Face ID unlock failed.";
  }
}

"use strict";
// pTask's Face ID wiring. The crypto and the refusals live in face-unlock.js (vendored,
// byte-identical with pNOC and pSCOPE); this file owns only the login shell's DOM.
//
// Two affordances, both on the auth gate that already asks for the dashboard password:
//   * enrolled  -> "Unlock with Face ID", which reveals the wrapped password and posts it to the
//                  same /api/auth/login the form posts to. The server learns nothing new.
//   * supported -> "Remember this device with Face ID", ticked by default, which enrols with the
//                  password just typed. Enrolment can only happen there, because that is the only
//                  moment the password is in hand.
//
// The markup is already in index.html and starts `hidden`; this module only unhides what the
// platform can actually deliver, so a device with no authenticator sees an unchanged login.

import {
  createFaceUnlock,
  faceUnlockMessage,
  indexedDbStore,
  signInWithFace,
  webauthnPrf,
} from "./face-unlock.js";

const APP_ID = "ptask";
const APP_NAME = "pTask";

const $ = (id) => document.getElementById(id);
const say = (text) => {
  const box = $("authError");
  if (box) box.textContent = text;
};

let faceUnlock = null;
try {
  faceUnlock = createFaceUnlock({
    prf: webauthnPrf({
      rpId: location.hostname,
      appId: APP_ID,
      appName: APP_NAME,
      userName: "operator",
      displayName: "pTask operator",
    }),
    store: indexedDbStore({ dbName: `${APP_ID}-face-unlock` }),
    appId: APP_ID,
    rpId: location.hostname,
    origin: location.origin,
  });
} catch {
  // No WebCrypto (a plain-http tailnet address, say). The password form is untouched.
}

async function postPassword(password) {
  const r = await fetch("/api/auth/login", {
    method: "POST",
    credentials: "include",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
  });
  return r.status;
}

function showUnlock(on) {
  const btn = $("faceUnlockBtn");
  const divider = $("faceDivider");
  if (btn) btn.hidden = !on;
  if (divider) divider.hidden = !on;
}

async function offerEnrolIfSupported() {
  const row = $("faceEnrolRow");
  if (!row || !faceUnlock) return;
  row.hidden = !(await faceUnlock.isSupported());
}

async function unlockWithFace() {
  const btn = $("faceUnlockBtn");
  if (!faceUnlock || !btn || btn.disabled) return;
  btn.disabled = true;
  say("");
  try {
    const { ok, forgotten } = await signInWithFace({ faceUnlock, signIn: postPassword });
    if (ok) {
      location.reload();
      return;
    }
    // Only a 401 means the dashboard password is dead. A lockout or an unreachable server keeps
    // the enrolment, because deleting it there would cost a good passkey for a transient failure.
    if (forgotten) {
      showUnlock(false);
      await offerEnrolIfSupported();
      say("Face ID needs setting up again on this device. Sign in with the password.");
    } else {
      say("Too many attempts, or the server is unreachable. Try again shortly.");
    }
  } catch (error) {
    say(faceUnlockMessage(error));
    const code = error && error.code;
    if (code === "stale-binding" || code === "version" || code === "decrypt-failed") {
      await faceUnlock.forget();
      showUnlock(false);
      await offerEnrolIfSupported();
    }
  }
  btn.disabled = false;
}

/**
 * Called by the login form with the password that just signed in. Resolves once enrolment has
 * been attempted, so the page reloads only after the platform sheet is done with the screen.
 */
async function afterPasswordLogin(password) {
  const box = $("faceEnrol");
  if (!faceUnlock || !box || !box.checked) return;
  try {
    await faceUnlock.enroll(password);
  } catch (error) {
    // The sign-in succeeded; only the convenience failed. Say so rather than swallow it, then let
    // the caller continue — the next sign-in offers enrolment again.
    say(`Signed in. ${faceUnlockMessage(error)}`);
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
}

window.ptaskFaceUnlock = { afterPasswordLogin, forget: () => faceUnlock?.forget() };

(async () => {
  if (!faceUnlock) return;
  const btn = $("faceUnlockBtn");
  if (btn) btn.addEventListener("click", unlockWithFace);
  if (await faceUnlock.isEnrolled()) {
    showUnlock(true);
    return;
  }
  await offerEnrolIfSupported();
})();

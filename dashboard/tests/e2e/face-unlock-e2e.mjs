// End-to-end verification of Face ID unlock against a real Chromium with a CDP virtual platform
// authenticator (PRF enabled). Drives the live pTask sidecar — the same server.py and index.html
// that ship — so this exercises the wrap, the enrolment, the unlock and the 401 currency refusal
// through the browser, not through a stub.
import { chromium } from '@playwright/test';

const BASE = process.env.BASE || 'http://localhost:9611';
const PASSWORD = process.env.PTASK_DASH_PASS;
const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ` — ${detail}` : ''}`);
};

const browser = await chromium.launch();
const context = await browser.newContext();
const page = await context.newPage();
const consoleErrors = [];
// A signed-out load 401s on /api/auth/check by design — that is how the shell decides to show the
// gate — and step 5 injects a 401 on /api/auth/login on purpose. Chrome logs both as console
// errors, so they are named here rather than blanket-ignored: anything else is a real regression.
const BY_DESIGN = /\/api\/auth\/(check|login)$/;
page.on('console', (m) => {
  if (m.type() !== 'error') return;
  const url = m.location()?.url || '';
  if (BY_DESIGN.test(url)) return;
  consoleErrors.push(`${m.text()} @ ${url}`);
});
page.on('pageerror', (e) => consoleErrors.push(String(e)));

const cdp = await context.newCDPSession(page);
await cdp.send('WebAuthn.enable');
const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
  options: {
    protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, isUserVerified: true,
    automaticPresenceSimulation: true, hasPrf: true,
  },
});
console.log(`virtual platform authenticator ${authenticatorId} (prf, uv, resident key)\n`);

// --- 1. a fresh device is offered enrolment, and is not offered an unlock it cannot perform
await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#authForm:not([hidden])', { timeout: 10000 });
check('login gate renders', await page.isVisible('#authForm'));
check('fresh device is offered enrolment', await page.isVisible('#faceEnrolRow'));
check('fresh device is NOT offered an unlock', !(await page.isVisible('#faceUnlockBtn')));
check('boot module loaded with no unexpected console errors', consoleErrors.length === 0, consoleErrors.join(' | '));

// --- 2. password sign-in enrols the device
await page.fill('#authPassword', PASSWORD);
await page.click('#authSubmit');
await page.waitForSelector('#authGate', { state: 'hidden', timeout: 15000 });
check('password sign-in reaches the dashboard', await page.isHidden('#authGate'));
const credentials = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
check('a resident platform credential was created', credentials.credentials.length === 1,
  `${credentials.credentials.length} credential(s)`);

// --- 3. the wrapped password is in IndexedDB and is not readable
const record = await page.evaluate(async () => {
  const db = await new Promise((res, rej) => {
    const r = indexedDB.open('ptask-face-unlock', 1);
    r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error);
  });
  return await new Promise((res, rej) => {
    const g = db.transaction('enrolment', 'readonly').objectStore('enrolment').get('record');
    g.onsuccess = () => res(g.result); g.onerror = () => rej(g.error);
  });
});
check('enrolment record persisted to IndexedDB', Boolean(record && record.version === 1));
check('record is bound to this rpId and origin',
  record?.rpId === 'localhost' && record?.origin === BASE, `${record?.rpId} / ${record?.origin}`);
check('record does not contain the password in the clear',
  !JSON.stringify(record).includes(PASSWORD) &&
  !Buffer.from(record.wrapped, 'base64').toString('binary').includes(PASSWORD));
const local = await page.evaluate(() => JSON.stringify(Object.entries(localStorage)));
check('nothing about the unlock is in localStorage', !local.includes('face') && !local.includes(PASSWORD));

// --- 4. a returning, signed-out device unlocks with Face ID alone
await context.clearCookies();
await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#authForm:not([hidden])');
check('returning device is offered Face ID', await page.isVisible('#faceUnlockBtn'));
check('returning device is not re-offered enrolment', !(await page.isVisible('#faceEnrolRow')));
await page.click('#faceUnlockBtn');
await page.waitForSelector('#authGate', { state: 'hidden', timeout: 15000 });
check('Face ID alone signs in', await page.isHidden('#authGate'));

// --- 5. REFUSAL currency: a rotated dashboard password (401) drops the enrolment
await context.clearCookies();
await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#faceUnlockBtn:not([hidden])');
await page.route('**/api/auth/login', (route) =>
  route.fulfill({ status: 401, contentType: 'application/json', body: '{"error":"authentication required"}' }));
await page.click('#faceUnlockBtn');
await page.waitForFunction(() => document.getElementById('authError')?.textContent?.includes('setting up again'), null, { timeout: 10000 });
check('401 shows the re-enrol message', true);
check('401 withdraws the Face ID button', await page.isHidden('#faceUnlockBtn'));
await page.unroute('**/api/auth/login');
await page.reload({ waitUntil: 'domcontentloaded' });
await page.waitForSelector('#authForm:not([hidden])');
check('the dropped enrolment stays dropped after a reload', !(await page.isVisible('#faceUnlockBtn')));
check('and the device is offered enrolment again', await page.isVisible('#faceEnrolRow'));

// --- 6. the password path still works with the enrolment gone
await page.fill('#authPassword', PASSWORD);
await page.click('#authSubmit');
await page.waitForSelector('#authGate', { state: 'hidden', timeout: 15000 });
check('password remains the fallback', await page.isHidden('#authGate'));

check('no unexpected console errors across the whole run', consoleErrors.length === 0, consoleErrors.join(' | '));
await browser.close();

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);

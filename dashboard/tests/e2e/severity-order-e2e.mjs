/* Verify the board actually renders severity-ordered.
 *
 * The defect this pins: `priority_score` used to be the primary sort key, so
 * the Critical panel could show a P2 above a P5. Asserted against a REAL
 * sidecar and a real Chromium — a passing unit test on the SQL string is not
 * evidence that the panel renders in that order.
 *
 * Driven by severity-order.sh, which boots the sidecar and passes BASE +
 * PTASK_DASH_PASS.
 */
import { chromium } from '@playwright/test';

const BASE = process.env.BASE;
const PASS = process.env.PTASK_DASH_PASS;
const SHOT = process.env.SHOT || '/tmp/ptask-severity-order.png';
if (!BASE || !PASS) {
  console.error('severity-order-e2e: BASE and PTASK_DASH_PASS are required');
  process.exit(2);
}

const fail = (msg) => { console.error('FAIL ' + msg); process.exitCode = 1; };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });

// The login gate probes /api/auth/check unauthenticated and aborts in-flight
// requests when it swaps the shell in, so only errors raised once the board is
// up are this test's business.
let boardUp = false;
const consoleErrors = [];
page.on('console', (m) => { if (boardUp && m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => { if (boardUp) consoleErrors.push('pageerror: ' + e.message); });
const failedRequests = [];
page.on('requestfailed', (r) => {
  const why = r.failure()?.errorText || '';
  if (boardUp && why !== 'net::ERR_ABORTED') failedRequests.push(r.url() + ' ' + why);
});

await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.fill('#authPassword', PASS);
await page.click('#authForm button[type="submit"]');
await page.waitForSelector('#crit .ccard', { timeout: 15000 });
boardUp = true;

// What the API says the order should be, straight from the same session.
const api = await page.evaluate(async () => {
  const r = await fetch('/api/tasks?status=pending&limit=5000', { credentials: 'same-origin' });
  return (await r.json()).tasks.map((t) => t.priority);
});

const critPriorities = await page.$$eval('#crit .ccard .pchip', (els) =>
  els.map((e) => Number(e.textContent.replace(/[^0-9]/g, ''))));

if (critPriorities.length === 0) fail('Critical panel rendered no cards');

for (let i = 1; i < critPriorities.length; i++) {
  if (critPriorities[i] > critPriorities[i - 1]) {
    fail(`Critical panel is not severity-ordered: P${critPriorities[i - 1]} then P${critPriorities[i]} `
       + `(full order: ${critPriorities.join(',')})`);
    break;
  }
}

const apiMax = Math.max(...api);
if (critPriorities[0] !== apiMax) {
  fail(`Critical panel leads with P${critPriorities[0]} but the highest pending severity is P${apiMax}`);
}

for (let i = 1; i < api.length; i++) {
  if (api[i] > api[i - 1]) {
    fail(`/api/tasks default order is not severity-first at index ${i}: P${api[i - 1]} then P${api[i]}`);
    break;
  }
}

// Lanes must hold only their own band — the grouping the operator reads down.
const laneMismatch = await page.$$eval('.lane-body', (bodies) => {
  for (const b of bodies) {
    const lane = Number(b.dataset.p);
    for (const chip of b.querySelectorAll('.pchip')) {
      const p = Number(chip.textContent.replace(/[^0-9]/g, ''));
      if (p !== lane) return `lane P${lane} contains a P${p} card`;
    }
  }
  return null;
});
if (laneMismatch) fail(laneMismatch);

await page.screenshot({ path: SHOT, fullPage: false });

if (consoleErrors.length) fail('console errors: ' + consoleErrors.join(' | '));
if (failedRequests.length) fail('failed requests: ' + failedRequests.join(' | '));

await browser.close();

if (process.exitCode) {
  console.error('severity-order-e2e: FAILED');
} else {
  console.log(`severity-order-e2e: PASS — critical panel ${critPriorities.join(',')}, `
            + `${api.length} pending tasks severity-ordered, lanes clean, no console errors`);
  console.log('screenshot: ' + SHOT);
}

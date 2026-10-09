/* Closure evidence and notes, in a real browser. Driven by notes.sh, which
 * boots a sidecar over a fresh DB with PT-1 "Rotate the backup key" (critical)
 * and PT-2 "Rack fox-n2" (urgent, one note by hal). */
import { chromium } from '@playwright/test';

const BASE = process.env.BASE;
const SHOT_DIR = process.env.SHOT_DIR || '/tmp';
if (!BASE) { console.error('notes-e2e: BASE is required'); process.exit(2); }

const fail = (msg) => { console.error('FAIL ' + msg); process.exitCode = 1; };
const consoleErrors = [];

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => consoleErrors.push('pageerror: ' + e.message));
await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('#crit .ccard', { timeout: 15000 });

// P1 with evidence: done → the dialog offers an empty Evidence field → the
// typed text rides with the completion.
const key = page.locator('#crit .ccard', { hasText: 'Rotate the backup key' }).first();
await key.locator('.btn-done').click();
await page.waitForSelector('#confirm:not([hidden])');
if ((await page.inputValue('#confirm-note')) !== '') fail('evidence field not empty on open');
if (!(await page.isVisible('label[for="confirm-note"]'))) fail('evidence field has no visible label');
await page.fill('#confirm-note', 'restic check: 0 errors');
await page.screenshot({ path: `${SHOT_DIR}/notes-confirm.png` });
await page.click('#confirm-ok');
await page.waitForFunction(() => /evidence noted/.test(document.querySelector('#toast')?.textContent || ''),
  null, { timeout: 10000 }).catch(() => fail('no "evidence noted" toast after done'));

// The dialog opens clean for the next task (no stale evidence carried over).
await page.waitForSelector('#crit .ccard', { timeout: 15000 });
const rack = page.locator('#crit .ccard', { hasText: 'Rack fox-n2' }).first();
await rack.locator('.btn-done').click();
await page.waitForSelector('#confirm:not([hidden])');
if ((await page.inputValue('#confirm-note')) !== '') fail('evidence from the last close leaked into the next');
await page.click('#confirm-cancel');

// The drawer shows the existing trail and adds to it.
await rack.locator('h3[data-act="drawer"]').click();
await page.waitForSelector('#drawer.open');
await page.waitForFunction(() => /rails ordered/.test(document.querySelector('#d-notes')?.textContent || ''),
  null, { timeout: 10000 }).catch(() => fail('drawer does not show the existing note'));
if (!/hal/.test(await page.textContent('#d-notes'))) fail('existing note is not attributed to hal');
await page.fill('#d-note', 'racked; IPMI answers on the tailnet');
await page.click('#d-note-add');
await page.waitForFunction(() => /IPMI answers/.test(document.querySelector('#d-notes')?.textContent || ''),
  null, { timeout: 10000 }).catch(() => fail('added note did not appear in the drawer'));
if ((await page.inputValue('#d-note')) !== '') fail('note box not cleared after adding');
const order = await page.$$eval('#d-notes .dnote .ntext', (ns) => ns.map((n) => n.textContent));
if (!(order.length === 2 && /IPMI/.test(order[0]))) fail(`notes not newest first: ${JSON.stringify(order)}`);
await page.screenshot({ path: `${SHOT_DIR}/notes-drawer.png` });
await page.keyboard.press('Escape');

// Phone width: the drawer is a bottom sheet; the note form fits without
// horizontal scroll.
await page.setViewportSize({ width: 390, height: 844 });
await page.waitForTimeout(400);
await page.locator('#crit .ccard', { hasText: 'Rack fox-n2' }).first().locator('h3[data-act="drawer"]').click();
await page.waitForSelector('#drawer.open');
await page.waitForSelector('#d-notes .dnote');
const overflow = await page.evaluate(() => {
  const dr = document.querySelector('#drawer');
  return { page: document.documentElement.scrollWidth - innerWidth, drawer: dr.scrollWidth - dr.clientWidth };
});
if (overflow.page > 0 || overflow.drawer > 0) fail(`horizontal overflow at 390px: ${JSON.stringify(overflow)}`);
await page.locator('#d-note-add').scrollIntoViewIfNeeded();
if (!(await page.isVisible('#d-note-add'))) fail('Add note button not reachable at 390px');
await page.screenshot({ path: `${SHOT_DIR}/notes-phone.png` });

if (consoleErrors.length) fail('console errors: ' + consoleErrors.join(' | '));
await browser.close();
if (!process.exitCode) console.log('notes-e2e: browser checks passed');

/* A second tenant runs the same cockpit with their own hats. Driven by
 * domains.sh, which boots a sidecar with a five-domain PTASK_DASH_DOMAINS list,
 * brand ALAN, default domain "personal", and two tasks: one labelled
 * domain:bretalon and one untagged. */
import { chromium } from '@playwright/test';

const BASE = process.env.BASE;
const PASS = process.env.PTASK_DASH_PASS;
const SHOT = process.env.SHOT || '/tmp/ptask-domains.png';
if (!BASE || !PASS) { console.error('domains-e2e: BASE and PTASK_DASH_PASS are required'); process.exit(2); }

const fail = (msg) => { console.error('FAIL ' + msg); process.exitCode = 1; };
const eq = (got, want, what) => { if (JSON.stringify(got) !== JSON.stringify(want)) fail(`${what}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`); };

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
let boardUp = false;
const consoleErrors = [];
page.on('console', (m) => { if (boardUp && m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => { if (boardUp) consoleErrors.push('pageerror: ' + e.message); });

await page.goto(BASE, { waitUntil: 'domcontentloaded' });
// brand follows config even on the login shell
await page.waitForFunction(() => document.querySelector('#authTitle')?.textContent.trim() === 'ALAN', null, { timeout: 10000 })
  .catch(() => fail('login title did not become ALAN'));
await page.fill('#authPassword', PASS);
await page.click('#authForm button[type="submit"]');
await page.waitForSelector('#crit .ccard', { timeout: 15000 });
boardUp = true;

eq(await page.$eval('#brandTitle', (e) => e.textContent.trim()), 'ALAN', 'header brand');
if (!(await page.title()).includes('ALAN')) fail('document.title does not carry the brand');

// the switch: ALL + the five configured hats, in config order, abbreviations shown
const switchAbbrs = await page.$$eval('#domains button', (bs) => bs.map((b) => b.textContent.replace(/\d+$/, '').trim()));
eq(switchAbbrs, ['ALL', 'PT', 'BRET', 'EAGLE', 'DILO', 'ME'], 'domain switch');
eq(await page.$$eval('#domains button', (bs) => bs.map((b) => b.dataset.d)),
   ['all', 'puretensor', 'bretalon', 'eaglestone', 'diloretio', 'personal'], 'domain switch keys');

// ALL shows both tasks; a hat filters to its own; the untagged task lives in the default hat
const visible = async () => page.$$eval('.ccard', (cs) => cs.map((c) => c.textContent).filter((t) => /Bretalon article|Windsor lease/.test(t)).length);
await page.click('#domains [data-d="all"]');
if ((await visible()) < 2) fail('ALL does not show both tasks');
await page.click('#domains [data-d="bretalon"]');
if ((await page.$$eval('.ccard', (cs) => cs.filter((c) => /Windsor lease/.test(c.textContent)).length)) !== 0) fail('BRET view still shows the untagged (personal) task');
if ((await page.$$eval('.ccard', (cs) => cs.filter((c) => /Bretalon article/.test(c.textContent)).length)) === 0) fail('BRET view does not show the bretalon task');
eq(await page.evaluate(() => document.documentElement.dataset.domain), 'bretalon', 'data-domain on <html>');
await page.click('#domains [data-d="personal"]');
if ((await page.$$eval('.ccard', (cs) => cs.filter((c) => /Windsor lease/.test(c.textContent)).length)) === 0) fail('ME (default domain) view does not show the untagged task');
if ((await page.$$eval('.ccard', (cs) => cs.filter((c) => /Bretalon article/.test(c.textContent)).length)) !== 0) fail('ME view leaks the bretalon task');

// the card chip wears the hat's abbreviation and a click moves the task to the NEXT hat,
// persisted as an explicit domain: label so it outlives a reload
await page.click('#domains [data-d="all"]');
const lease = page.locator('.ccard', { hasText: 'Windsor lease' }).first();
eq((await lease.locator('.dtag').first().textContent()).trim(), 'ME', 'chip on the untagged task');
await lease.locator('.dtag').first().click();
await page.waitForTimeout(1500);
const labels = await page.evaluate(async () => {
  const r = await fetch('/api/tasks?status=pending&limit=50', { credentials: 'same-origin' });
  const t = (await r.json()).tasks.find((x) => /Windsor lease/.test(x.title));
  return (t.labels || []).filter((l) => l.startsWith('domain:'));
});
eq(labels, ['domain:puretensor'], 'chip click persists the next hat (wraps personal -> puretensor)');
await page.click('#domains [data-d="puretensor"]');
if ((await page.$$eval('.ccard', (cs) => cs.filter((c) => /Windsor lease/.test(c.textContent)).length)) === 0) fail('moved task not shown under PT');

// composer picker: AUTO + the five hats
await page.click('#domains [data-d="all"]');
await page.click('#cap-type');
await page.waitForSelector('#c-dom button', { timeout: 5000 }).catch(() => fail('composer domain picker did not render'));
eq(await page.$$eval('#c-dom button', (bs) => bs.map((b) => b.dataset.v)),
   ['auto', 'puretensor', 'bretalon', 'eaglestone', 'diloretio', 'personal'], 'composer picker');

// nothing engineering-flavoured survives in the tenant view
const body = await page.evaluate(() => document.body.innerText);
if (/\bENG\b|\bMGMT\b|Engineering|Management/.test(body)) fail('ENG/MGMT vocabulary leaked into a configured-domains instance');

await page.screenshot({ path: SHOT, fullPage: true });
if (consoleErrors.length) fail('console errors: ' + consoleErrors.join(' | '));
await browser.close();
console.log(process.exitCode ? 'domains-e2e: FAIL' : 'domains-e2e: PASS (' + SHOT + ')');

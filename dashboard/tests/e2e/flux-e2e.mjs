/* The flux range picker's per-actor block, in a real browser at desktop and
 * phone width. Driven by flux.sh: five tasks opened by hal, one closed by
 * hal, one closed and one dismissed by shell. */
import { chromium } from '@playwright/test';
const BASE = process.env.BASE, SHOT = process.env.SHOT || '/tmp/ptask-flux';
if (!BASE) { console.error('flux-e2e: BASE is required'); process.exit(2); }
const fail = (m) => { console.error('FAIL ' + m); process.exitCode = 1; };
const browser = await chromium.launch();
for (const [w, h, name] of [[1440, 900, 'desk'], [390, 844, 'phone']]) {
  const page = await browser.newPage({ viewport: { width: w, height: h } });
  const errs = []; page.on('pageerror', (e) => errs.push(e.message));
  await page.goto(BASE, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-act="flux"]:visible', { timeout: 15000 });
  await page.locator('[data-act="flux"]:visible').first().click();
  await page.waitForSelector('#pmenu:not([hidden]) .pma', { timeout: 5000 }).catch(() => fail(name + ': no per-actor rows'));
  const rows = await page.$$eval('#pmenu .pma', (rs) => rs.map((r) => r.textContent.replace(/\s+/g, ' ').trim()));
  console.log(name, JSON.stringify(rows));
  if (!rows[0] || !rows[0].startsWith('hal')) fail(name + ': hal (largest net) not first');
  const up = await page.$eval('#pmenu .pma .net', (e) => e.className);
  if (!/up/.test(up)) fail(name + ': positive net not flagged');
  const over = await page.evaluate(() => { const m = document.querySelector('#pmenu').getBoundingClientRect(); return m.right > innerWidth || m.left < 0; });
  if (over) fail(name + ': popover off-screen');
  // Arrow keys still walk only the window items.
  await page.keyboard.press('ArrowDown');
  const focused = await page.evaluate(() => document.activeElement?.className || '');
  if (!/pmi/.test(focused)) fail(name + ': arrow focus left the window items: ' + focused);
  await page.screenshot({ path: `${SHOT}-${name}.png` });
  if (errs.length) fail(name + ': ' + errs.join(' | '));
  await page.close();
}
await browser.close();
if (!process.exitCode) console.log('flux e2e: ok');

// @ts-check
// The admin Query page in a real browser against the real binaries: without
// JavaScript every part is a form or a link (Run, a refused write, the
// export link); with it Ctrl/⌘+Enter swaps only the results and keeps the
// URL, picking the engine swaps the tables, and a long value opens in a
// dialog. On a phone nothing scrolls the page sideways. Its stages go in the
// coverage gallery's Logs group (the admin's tools).
const { test, expect } = require('../coverage-test');
const { captureCoverageStage } = require('../coverage-screenshot');
const { useRealStack, fixture, signInAsAdmin } = require('./backend-helpers');

useRealStack(test);

const queryUrl = (db = 'monokulo', sql = '') =>
  `${fixture().monokulo_url}/admin/query?db=${db}&sql=${encodeURIComponent(sql)}`;
const stage = (page, name) => captureCoverageStage(page, name, test.info(), { group: 'logs' });

// Twenty-four columns, and a long JSON value beside a blob.
const WIDE = 'SELECT ' + Array.from({ length: 24 }, (_, i) => `${i} AS column_${i}`).join(', ');
const BIG = `SELECT 'wd_1a2b' AS id, '{"event":"order.paid","order":{"id":"o1","note":"' || hex(zeroblob(300)) || '"}}' AS payload, zeroblob(64) AS signature`;

test('without JavaScript a query runs, a write is refused, and the rows download', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await signInAsAdmin(page);
  await page.goto(queryUrl('monokulo', 'SELECT email, password_hash FROM users'));
  const grid = page.locator('.mk-data-grid');
  await expect(grid).toContainText('admin@example.com');
  await expect(grid.locator('td[data-kind="hidden"]')).toHaveText('hidden');
  await expect(page.locator('.mk-data-grid-stats')).toContainText('1 column hidden');
  await expect(page.getByRole('link', { name: 'Export CSV' })).toHaveAttribute('href', /\/admin\/query\/export\?db=monokulo&sql=.*&format=csv/);

  // Run is an ordinary GET form.
  await page.locator('#query-sql').fill('DELETE FROM users');
  await page.getByRole('button', { name: 'Run' }).click();
  await expect(page.locator('#query-results')).toContainText('Not run: this page only reads.');
  await expect(page).toHaveURL(/sql=DELETE/);
  await page.goto(queryUrl('monokulo', 'SELECT count(*) AS admins FROM users'));
  await expect(page.locator('.mk-data-grid')).toContainText('1');

  // A long value opens on a page of its own.
  await page.goto(queryUrl('monokulo', BIG));
  await page.getByRole('link', { name: 'Expand payload of row 1' }).click();
  await expect(page.locator('h1')).toContainText('payload');
  await expect(page.locator('pre')).toContainText('"event": "order.paid"');
  await context.close();
});

test('with JavaScript Ctrl+Enter swaps the results, and the engine swaps the tables', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(queryUrl());
  await expect(page.locator('.query-empty')).toContainText('Pick a table on the left');
  await stage(page, 'query-empty');
  // A marker that only survives if the page is never reloaded.
  await page.evaluate(() => { window.__notReloaded = true; });

  await page.locator('#query-sql').fill(WIDE);
  await page.locator('#query-sql').press('Control+Enter');
  await expect(page.locator('.mk-data-grid-stats')).toContainText('24 columns');
  await expect(page.locator('.mk-data-grid-hint')).toContainText('Scroll sideways for more columns');
  await expect(page).toHaveURL(/sql=SELECT/);
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  // The grid scrolls inside its own box; the page doesn't.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await stage(page, 'query-wide');

  await page.locator('#query-sql').fill(BIG);
  await page.locator('#query-sql').press('Control+Enter');
  await expect(page.locator('.mk-data-cell[data-kind="blob"]')).toContainText('‹blob 64 B›');
  await page.getByRole('link', { name: 'Expand payload of row 1' }).click();
  const dialog = page.locator('#expand-value');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('pre')).toContainText('"event": "order.paid"');
  await stage(page, 'query-expanded');
  await dialog.getByRole('button', { name: 'Close' }).last().click();
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

  await page.locator('#query-sql').fill('UPDATE users SET email = email');
  await page.locator('#query-sql').press('Control+Enter');
  await expect(page.locator('#query-results')).toContainText('Not run: this page only reads.');
  await stage(page, 'query-refused');

  // Picking the engine swaps the tables beside an empty editor.
  await page.locator('#query-db').selectOption('engine', { force: true });
  await expect(page.locator('.mk-table-browser [data-table="tenants"]')).toBeVisible();
  await expect(page.locator('#query-sql')).toHaveValue('');
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  // The filter narrows the list.
  await page.getByLabel('Filter tables').fill('tena');
  await expect(page.locator('.mk-table-browser [data-table="orders"]')).toBeHidden();
  await expect(page.locator('.mk-table-browser [data-table="tenants"]')).toBeVisible();

  await page.locator('#query-sql').fill('SELECT id, secret_token_hash FROM tenants');
  await page.getByRole('button', { name: 'Run' }).click();
  await expect(page.locator('#query-results')).toContainText('engine');
  await stage(page, 'query-engine');
});

test('on a phone the editor comes first, the tables fold, and nothing scrolls sideways', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signInAsAdmin(page);
  await page.goto(queryUrl('monokulo', WIDE));
  await expect(page.locator('.mk-data-grid')).toBeVisible();
  const tables = page.locator('details.query-tables');
  await expect(tables).not.toHaveAttribute('open', '');
  await expect(tables.locator(':scope > summary')).toContainText(/Tables \(\d+\)/);
  const editor = await page.locator('.query-editor').boundingBox();
  const summary = await tables.locator(':scope > summary').boundingBox();
  expect(summary.y).toBeGreaterThan(editor.y + editor.height);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await stage(page, 'query-phone');
});

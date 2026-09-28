// Shared by the tests/real-*.spec.js specs, which run one after another
// against the same processes (real-binaries-setup.js), so each copes with
// whatever state the one before left.
const { expect } = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');

const fixture = () => JSON.parse(fs.readFileSync(path.join(__dirname, '..', '.real-binaries-fixture.json'), 'utf8'));

const ADMIN_EMAIL = 'admin@example.com';
const ADMIN_PASSWORD = 'correct horse battery staple';

// The dev stagenet wallet (scripts/dev-run.sh): a watch-only key pair.
const VIEW_KEY = 'fcdc7998f003928b3f409b94d54f690d16ca6df3689de4da4803c5a9c792fb0e';
const SPEND_PUBKEY = '3fa2161d4e2cc7722288d33e46a4cc37e92629d7e45939ec67cc42e8f144b335';

/** Signs in as the instance admin, creating the account on a fresh instance. */
async function signInAsAdmin(page) {
  const { monokulo_url: base } = fixture();
  await page.goto(base + '/');
  if (page.url().includes('/admin/setup')) {
    await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
    await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
    await page.locator('input[name="confirm_password"]').fill(ADMIN_PASSWORD);
    await page.getByRole('button', { name: 'Create admin account' }).click();
    return;
  }
  await page.goto(base + '/dashboard/login');
  await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
  await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await page.locator('form[action="/dashboard/login"] button[type="submit"]').click();
}

/** The fake monerod as a `monero_node` JSON value. */
function fakeNodeJson() {
  const [host, port] = fixture().fake_monerod.split(':');
  return JSON.stringify({ host, port: Number(port), ssl: false, accept_self_signed_certs: true, fallbacks: [] });
}

/** Saves engine settings from the admin page; `fields` maps input names to values. */
async function saveEngineSettings(page, fields) {
  const { monokulo_url: base } = fixture();
  await page.goto(base + '/dashboard/admin/settings');
  // Lists of choices first: ticking a key custody backend shows its own
  // section, whose fields can then be filled.
  const isList = async (name) => (await page.locator(`input[type=checkbox][name="${name}"]`).count()) > 0;
  const entries = Object.entries(fields);
  const lists = [];
  for (const entry of entries) if (await isList(entry[0])) lists.push(entry);
  for (const [name, value] of lists) {
    const chosen = value.split(',').map((v) => v.trim());
    for (const box of await page.locator(`input[type=checkbox][name="${name}"]`).all()) {
      await box.setChecked(chosen.includes(await box.getAttribute('value')));
    }
  }
  for (const [name, value] of entries.filter((entry) => !lists.includes(entry))) {
    const field = page.locator(`[name="${name}"]`);
    if ((await field.evaluate((el) => el.tagName)) === 'SELECT') await field.selectOption(value);
    else await field.fill(value);
  }
  await page.getByRole('button', { name: 'Save engine settings' }).click();
}

// monokulo caches the engine's status for up to 10s, so a page reflects a
// change within a few reloads.
async function reloadUntil(page, url, check) {
  await expect
    .poll(async () => {
      await page.goto(url);
      return check(await page.content());
    }, { timeout: 30_000, intervals: [1000] })
    .toBe(true);
}

/** Connects a stagenet store for `site` (giving stagenet the fake node
 * first) and returns its dashboard path, `/dashboard/stores/{id}`. */
async function connectStore(page, site) {
  const { monokulo_url: base } = fixture();
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
  await page.goto(base + '/dashboard/connect');
  await page.locator('input[name="site_url"]').fill(`https://${site}`);
  await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
  await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
  await page.locator('select[name="network"]').selectOption('stagenet');
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
  await page.goto(base + '/dashboard');
  return page.locator('tr', { hasText: site }).first().getByRole('link', { name: 'view →' }).getAttribute('href');
}

module.exports = { fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, reloadUntil, connectStore, VIEW_KEY, SPEND_PUBKEY };

// Shared by the the specs selected by real-binaries.config.js specs. Each spec file calls
// `useRealStack(test)` and gets processes of its own (real-stack.js): its
// tests run one after another against them, so each copes with whatever
// state the one before in the same file left, and files never see each
// other's state, whichever worker runs them.
const { expect } = require('@playwright/test');
const { startStack } = require('../real-stack');

let stack = null;

/** Starts a fresh stack before the file's first test and stops it after its last. */
function useRealStack(test) {
  test.beforeAll(async () => { stack = await startStack(); });
  test.afterAll(async () => {
    const running = stack;
    stack = null;
    if (running) await running.stop();
  });
}

/** The running stack: `{ monokulo_url, engine_url, fake_monerod, logs }`. */
function fixture() {
  if (!stack) throw new Error('no real-binaries stack is running: call useRealStack(test) at the top of the spec');
  return stack.fixture;
}

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

/** The fake monerod's address, as the node form takes it. */
function fakeNodeAddress() {
  return fixture().fake_monerod;
}

/** A network's node rows on the open Monero nodes tab (address boxes, the blank "Add a node" row last). */
function nodeAddressBoxes(page, network) {
  return page.locator(`[data-network="${network}"] input[name^="node_${network}_"][name$="_address"]`);
}

/**
 * Fills the open Monero nodes tab so each network given has exactly these
 * nodes, in order (`[]` clears it): the rows there are refilled, extra ones
 * blanked (a blank row is dropped on save), and the blank row takes one
 * more. Doesn't save.
 */
async function fillNodes(page, nodes) {
  for (const [network, addresses] of Object.entries(nodes)) {
    // A network with no nodes and no stores starts closed, and so does
    // each of its node rows.
    await openNodes(page, network);
    const boxes = nodeAddressBoxes(page, network);
    const count = await boxes.count();
    if (addresses.length > count) throw new Error(`only ${count} rows for ${network}`);
    for (let i = 0; i < count; i += 1) await boxes.nth(i).fill(addresses[i] || '');
  }
}

/** Opens a network's card on the Monero nodes tab, and each of its node rows. */
async function openNodes(page, network) {
  await page.locator(`[data-network="${network}"] details`).evaluateAll((all) => all.forEach((d) => { d.open = true; }));
}

/**
 * Presses Save on the save bar. With JavaScript the bar only shows once
 * something on the tab changed: with nothing changed, the form is sent as
 * Enter would send it, and the toast says there was nothing to save.
 */
async function pressSave(page) {
  // The last save's toast goes first, so `expectSaved` waits for this one's.
  await page.locator('#settings-toasts .toast').evaluateAll((all) => all.forEach((toast) => toast.remove()));
  const save = page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true });
  if (await save.isVisible()) await save.click();
  else await page.locator('#settings-form').evaluate((form) => form.requestSubmit(form.querySelector('[data-save]')));
}

/** Waits for the toast a save leaves: saved (green, or amber for a restart it waits for), or nothing to save. */
async function expectSaved(page) {
  await expect(page.locator('#settings-toasts').locator('.toast-success, .toast-warning, .toast-neutral').first()).toBeVisible();
}

/** Sets networks' nodes on the Monero nodes tab and saves, e.g. `{ stagenet: [fakeNodeAddress()] }`. */
async function saveNodes(page, nodes) {
  await openSettingsTab(page, 'nodes');
  await fillNodes(page, nodes);
  await pressSave(page);
}

/**
 * The admin settings tab (its `?tab=` id) a form field is on, by its name:
 * the same map as `setting_placement` in crates/monokulo/src/views/admin.rs.
 */
function settingsTabOf(name) {
  const key = name.replace(/^clear:/, '');
  if (key.startsWith('node_')) return 'nodes';
  if (key === 'payment.scan_chunk_memory_budget_mb') return 'server';
  if (/^(payment|webhooks|exchange_rate)\./.test(key)) return 'payments';
  if (key.startsWith('key_custody.')) return 'custody';
  if (/^(abuse|rate_limit)\./.test(key)) return 'abuse';
  if (/^(server|http_cache)\./.test(key)) return 'server';
  if (/^(engine:)?logging\./.test(key)) return 'logging';
  return 'general';
}

/** Every tab of the admin settings page, in the tab bar's order (Other only shows when it has something). */
const SETTINGS_TABS = ['general', 'nodes', 'payments', 'custody', 'abuse', 'server', 'logging'];

/** Opens one tab of the admin settings page. */
async function openSettingsTab(page, tab) {
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings?tab=${tab}`);
}

/** Fills the open tab's fields; `fields` maps input names to values. */
async function fillSettings(page, fields) {
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
}

/**
 * Saves settings from the admin page; `fields` maps input names to values.
 * Opens the tab holding them and presses its Save; fields on several tabs
 * are saved one tab after another, each save finished before the next.
 */
async function saveEngineSettings(page, fields) {
  const byTab = new Map();
  for (const [name, value] of Object.entries(fields)) {
    const tab = settingsTabOf(name);
    if (!byTab.has(tab)) byTab.set(tab, {});
    byTab.get(tab)[name] = value;
  }
  const tabs = [...byTab.keys()];
  for (const [i, tab] of tabs.entries()) {
    await openSettingsTab(page, tab);
    await fillSettings(page, byTab.get(tab));
    await pressSave(page);
    if (i < tabs.length - 1) await expect(page.locator('#settings-toasts .toast')).toBeVisible();
  }
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

/** Completes the optional common settings form and returns the store path. */
async function finishStoreSetup(page) {
  await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
  await page.getByRole('button', { name: 'Skip for now', exact: true }).click();
  await expect(page).toHaveURL(/\/dashboard\/stores\/[^/]+$/);
  return new URL(page.url()).pathname;
}

/** Connects a stagenet store for `site` (giving stagenet the fake node
 * first) and returns its dashboard path, `/dashboard/stores/{id}`. */
async function connectStore(page, site) {
  const { monokulo_url: base } = fixture();
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await page.goto(base + '/dashboard/connect');
  await page.locator('input[name="site_url"]').fill(`https://${site}`);
  await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
  await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
  await page.locator('select[name="network"]').selectOption('stagenet');
  await page.getByRole('button', { name: 'Connect' }).click();
  await finishStoreSetup(page);
  await page.goto(base + '/dashboard');
  return page.locator('tr', { hasText: site }).first().getByRole('link', { name: 'view →' }).getAttribute('href');
}

/**
 * Waits for the page's view transition to finish. Every navigation from a
 * link or form runs the theme indicator's cross-document view transition
 * (site.css), and while it runs the page takes no clicks. With JavaScript
 * off, a click must not land mid-transition: Playwright waits between
 * retries with a timer in the page, which never fires, so the click hangs
 * until the test times out.
 */
async function transitionDone(page) {
  await expect.poll(() => page.evaluate(() => !document.activeViewTransition)).toBe(true);
}

module.exports = {
  useRealStack, fixture, signInAsAdmin, transitionDone, fakeNodeAddress, saveNodes, fillNodes, nodeAddressBoxes, saveEngineSettings,
  settingsTabOf, openSettingsTab, fillSettings, openNodes, pressSave, expectSaved,
  SETTINGS_TABS, reloadUntil, connectStore, finishStoreSetup, VIEW_KEY, SPEND_PUBKEY,
};

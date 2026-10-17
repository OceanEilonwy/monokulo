// @ts-check
// The admin settings page in a real browser (admin_settings_v2.md 4.4, 4.5
// and 4.7), served by the real binaries rather than the coverage fixture:
// descriptions and examples at phone and desktop widths, the confirmation
// before clearing a network stores use, and banners in the error colour in
// both themes (a save's own result is a toast; a banner says what stays
// true after it).
const { test, expect } = require('@playwright/test');
const {
  createStore, useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, fillNodes, saveEngineSettings, openSettingsTab, SETTINGS_TABS, VIEW_KEY, SPEND_PUBKEY, expectSaved, openNodes, pressSave,
  answerNodeQuestion,
} = require('./backend-helpers');

useRealStack(test);

// Whatever a test did to the stagenet node, the next one starts with it set.
test.afterEach(async ({ page }) => {
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
});

const SIZES = { 'small phone': { width: 320, height: 568 }, phone: { width: 390, height: 844 }, desktop: { width: 1280, height: 900 } };

for (const [name, size] of Object.entries(SIZES)) {
  test(`every setting is described, with examples, and nothing scrolls sideways (${name})`, async ({ page }) => {
    await page.setViewportSize(size);
    await signInAsAdmin(page);
    let total = 0;
    for (const tab of SETTINGS_TABS) {
      await openSettingsTab(page, tab);
      // A key custody backend that's turned off keeps its section hidden.
      const fields = page.locator('.setting-field:visible');
      total += await fields.count();
      for (const field of await fields.all()) {
        await expect(field.locator('.field-help').first()).toBeVisible();
      }
      if (tab === 'nodes') {
        // A network with no nodes and no stores starts closed, and so does
        // each node row; opened, its address help carries an example
        // address for that network.
        await openNodes(page, 'stagenet');
        await expect(page.locator('[data-network="stagenet"] .field-help code').first()).toBeVisible();
      }
      const sideways = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(sideways, tab).toBeLessThanOrEqual(0);
    }
    expect(total).toBeGreaterThan(20);
  });
}

test('clearing a network stores use asks first; one no store uses does not', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  // Testnet gets a node no store uses. The fake node says it's on
  // stagenet, so testnet gets one that doesn't answer, which is saved.
  await saveNodes(page, { stagenet: [fakeNodeAddress()], testnet: ['127.0.0.1:9'] });
  await expectSaved(page);

  await openSettingsTab(page, 'nodes');
  const stagenet = page.locator('.node-network[data-network="stagenet"]');
  if (Number(await stagenet.getAttribute('data-tenant-count')) === 0) {
    // Run on its own: make a store on stagenet to protect.
    await createStore(page, 'guarded.example.com');
    await expect(page.getByRole('heading', { name: 'guarded.example.com is set up' })).toBeVisible();
    await openSettingsTab(page, 'nodes');
  }
  const count = Number(await stagenet.getAttribute('data-tenant-count'));
  expect(count).toBeGreaterThan(0);
  const expected = count === 1 ? '1 store uses the stagenet network' : `${count} stores use the stagenet network`;
  let posts = 0;
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/dashboard/admin/settings')) posts += 1;
  });

  // Dismissed: nothing is sent. Proven by the accepted save below being
  // the only post: one the question had sent would have gone out first.
  await fillNodes(page, { stagenet: [] });
  await pressSave(page);
  expect(await answerNodeQuestion(page, 'Keep editing')).toContain(expected);

  // Saved anyway: sent, and the red banner says what happened.
  await pressSave(page);
  await answerNodeQuestion(page, 'Save anyway');
  await expect(page.getByText(/the stagenet network, which no longer has any reachable nodes/)).toBeVisible();
  expect(posts).toBe(1);

  // Testnet has no stores: no question.
  await fillNodes(page, { testnet: [] });
  await pressSave(page);
  await expectSaved(page);
  await expect(page.locator('#node-clear-dialog')).toBeHidden();
  expect(posts).toBe(2);

  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
});

for (const scheme of ['light', 'dark']) {
  for (const [name, size] of Object.entries(SIZES)) {
    test(`banners fit and the red one uses the error colour (${scheme}, ${name})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.setViewportSize(size);
      await signInAsAdmin(page);
      // A red banner (a network stores use cleared) and a yellow one (a
      // restart-only setting) on one page.
      await openSettingsTab(page, 'server');
      const threads = Number(await page.locator('input[name="server.worker_threads"]').inputValue());
      // The restart-only setting first, then the node: the banners after
      // the second save are the node's; the restart shows on its field.
      await saveEngineSettings(page, { 'server.worker_threads': String((threads % 8) + 1) });
      await expect(page.locator('#settings-toasts .toast-warning')).toBeVisible();
      await saveNodes(page, { stagenet: [] });
      await answerNodeQuestion(page, 'Save anyway');
      const red = page.locator('p.error[role="alert"]').first();
      await expect(red).toBeVisible();
      const [color, token] = await red.evaluate((el) => {
        const probe = document.createElement('span');
        probe.style.color = 'var(--error)';
        document.body.appendChild(probe);
        const expected = getComputedStyle(probe).color;
        probe.remove();
        return [getComputedStyle(el).color, expected];
      });
      expect(color).toBe(token);
      // theme.css's --error for this theme.
      expect(color).toBe(scheme === 'dark' ? 'rgb(255, 107, 107)' : 'rgb(176, 0, 32)');
      const sideways = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(sideways).toBeLessThanOrEqual(0);
      await saveNodes(page, { stagenet: [fakeNodeAddress()] });
      await expectSaved(page);
    });
  }
}

test('an on/off setting is a switch: on and off both save', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'abuse');
  const input = page.locator('input[role=switch][name="abuse.under_attack"]');
  const control = page.locator('.switch', { has: input });
  await expect(control).toBeVisible();
  await expect(page.getByRole('switch', { name: /under attack/i })).toHaveCount(1);
  const save = async () => {
    await pressSave(page);
    await expectSaved(page);
    await openSettingsTab(page, 'abuse');
  };

  await expect(input).not.toBeChecked();
  await expect(control.locator('.switch-off')).toBeVisible();
  // At the right edge of its setting, where the other controls end.
  const [box, field] = await Promise.all([
    control.boundingBox(),
    page.locator('.setting-field').filter({ has: input }).last().boundingBox(),
  ]);
  expect(Math.abs(box.x + box.width - (field.x + field.width))).toBeLessThan(1);
  await control.click();
  await expect(input).toBeChecked();
  await expect(control.locator('.switch-on')).toBeVisible();
  await save();
  await expect(input).toBeChecked();

  // Off sends no value of its own; the page still saves it as off.
  await control.click();
  await save();
  await expect(input).not.toBeChecked();
});

// The engine's limit on monokulo's own calls: a switch, and its count,
// which can only be changed (and is only sent) while the switch is on.
// This stack's engine is standalone, where the limit is on.
test('the count of monokulo\'s own requests follows its switch, and saves either way', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'server');
  const card = page.locator('#card-server-engine');
  const toggle = card.getByRole('switch', { name: "Limit monokulo's own requests" });
  const count = card.getByRole('spinbutton', { name: "Requests a minute for monokulo's own calls" });
  await expect(toggle).toBeChecked();
  await expect(count).toBeEnabled();
  await expect(card.getByText('Off when the engine runs inside monokulo: only monokulo can call it.')).toBeVisible();
  await expect(card.getByRole('spinbutton', { name: 'Requests a minute for each store' })).toBeVisible();

  const shots = process.env.RATE_LIMIT_SHOTS;
  const shoot = async (label) => {
    if (!shots) return;
    for (const scheme of ['light', 'dark']) {
      await page.emulateMedia({ colorScheme: scheme });
      for (const width of [1280, 390]) {
        await page.setViewportSize({ width, height: 900 });
        await card.screenshot({ path: `${shots}/server-engine-${label}-${scheme}-${width}.png` });
      }
    }
    await page.emulateMedia({ colorScheme: 'light' });
    await page.setViewportSize({ width: 1280, height: 900 });
  };
  await shoot('on');

  // Off: the count is disabled at once, and Discard puts both back.
  await card.locator('.switch', { has: toggle }).click();
  await expect(toggle).not.toBeChecked();
  await expect(count).toBeDisabled();
  await card.getByRole('button', { name: 'Discard' }).click();
  await expect(toggle).toBeChecked();
  await expect(count).toBeEnabled();

  // Saved off, the count stays as it was and comes back disabled.
  const before = await count.inputValue();
  await card.locator('.switch', { has: toggle }).click();
  await pressSave(page);
  await expectSaved(page);
  await openSettingsTab(page, 'server');
  await expect(toggle).not.toBeChecked();
  await expect(count).toBeDisabled();
  await expect(count).toHaveValue(before);
  await shoot('off');

  // On again, with a count of its own.
  await card.locator('.switch', { has: toggle }).click();
  await expect(count).toBeEnabled();
  await count.fill('50000');
  await pressSave(page);
  await expectSaved(page);
  await openSettingsTab(page, 'server');
  await expect(toggle).toBeChecked();
  await expect(count).toHaveValue('50000');
  await saveEngineSettings(page, { 'server.instance_rate_limit_per_min': before });
  await expectSaved(page);
});

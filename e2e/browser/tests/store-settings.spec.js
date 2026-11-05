// A store's settings page (crates/monokulo/src/views/store_settings.rs): seven
// tabs, each a page of its own (`?tab=`). General's and Payments' settings
// are cards in one form with one save bar (the settings components,
// crates/ui); the wallet, the keys, the domains (domains.spec.js) and the
// webhooks are cards of their own on the Wallet, Domains and Webhooks tabs. Every
// save works without JavaScript too.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
let fixture;
// This file's tests share one fixture and the settings they save, so
// they run in order on one worker.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const store = '/stores/coverage-store';
const tab = (name) => `${fixture.base_url}${store}/settings?tab=${name}`;

/** Records the settings components' events, across page loads. */
async function recordEvents(page) {
  await page.addInitScript(() => {
    window.__mk = [];
    for (const name of ['mk-dirty', 'mk-saved', 'mk-failed']) {
      document.addEventListener(name, (event) => window.__mk.push({ name, detail: event.detail }));
    }
  });
}
const events = (page, name) => page.evaluate((name) => window.__mk.filter((e) => e.name === name).map((e) => e.detail), name);

test('a save says so on the card and in a toast; a refused one keeps what was typed', async ({ page, context }) => {
  await login(context);
  await recordEvents(page);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(tab('general'));
  await expect(page.locator('.mk-tabs[aria-label="Store settings"] a[aria-current="page"]')).toHaveText('General');
  const bar = page.locator('#save-bar');
  const card = page.locator('#card-store');
  const field = card.locator('input[name="payment_description"]');
  const saved = await field.inputValue();
  await expect(bar).toBeHidden();
  // No dialog open: every setting is in its card; only the website's
  // confirm dialogs wait, closed.
  await expect(page.locator('dialog[open]')).toHaveCount(0);
  await expect(page.locator('dialog:not(.confirm-dialog)')).toHaveCount(0);
  await captureCoverageStage(page, 'store-settings', test.info());

  await field.fill('Coffee and cake');
  await expect(card).toHaveClass(/is-dirty/);
  await expect(card.locator('[data-unsaved]')).toHaveText('1 unsaved');
  await expect(card.locator('.setting-field.is-changed .changed-mark').first()).toBeVisible();
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('1 unsaved change in Store');
  expect((await events(page, 'mk-dirty')).at(-1)).toEqual({ count: 1, groups: ['store'] });
  await captureCoverageStage(page, 'store-settings-dirty', test.info());

  // A second card's change is named too.
  await page.locator('#card-diagnostics label.switch').click();
  await expect(bar).toContainText('2 unsaved changes in Store and Diagnostics');

  await card.getByRole('button', { name: 'Discard' }).click();
  await expect(field).toHaveValue(saved);
  await expect(card).not.toHaveClass(/is-dirty/);
  await expect(bar).toContainText('1 unsaved change in Diagnostics');
  await bar.getByRole('link', { name: 'Discard changes' }).click();
  await expect(bar).toBeHidden();
  await expect(page.locator('#card-diagnostics input[role="switch"]')).not.toBeChecked();
  expect((await events(page, 'mk-dirty')).at(-1)).toEqual({ count: 0, groups: [] });

  // An action on the Webhooks tab: its card says what it did, once.
  await page.goto(tab('webhooks'));
  const webhooks = page.locator('#card-webhooks');
  await webhooks.locator('input[name="url"]').fill('https://hooks.example.com/monokulo');
  await webhooks.getByRole('button', { name: 'Add webhook' }).click();
  await expect(webhooks.getByRole('heading', { name: 'Webhook created' })).toBeVisible();
  await expect(page.locator('#settings-toasts')).toContainText('Webhook created');

  // The payments tab: a save refused, then one saved.
  await page.goto(tab('payments'));
  const thresholds = page.locator('#card-confirmation-thresholds');
  const confirmations = () => page.locator('#card-confirmation-thresholds input[name="confirmations_required"]');
  const threshold = await confirmations().inputValue();

  // Refused: nothing saved, the card says why, what was typed stays.
  await confirmations().fill('721');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(thresholds).toHaveClass(/is-failed/);
  await expect(thresholds.locator('.card-body > p.error')).toContainText('from 0 to 720');
  await expect(confirmations()).toHaveValue('721');
  await expect(bar).toBeVisible();
  await expect(bar).toHaveClass(/is-failed/);
  await expect(bar).toContainText('Nothing saved.');
  await expect(page.locator('#card-webhooks [role="alert"]')).toHaveCount(0);
  await expect(page.locator('#settings-toasts .toast-error')).toContainText('from 0 to 720');
  expect(await events(page, 'mk-failed')).toEqual([{ group: 'confirmation-thresholds', message: 'Enter a whole number of confirmations from 0 to 720.' }]);
  await captureCoverageStage(page, 'store-settings-failed', test.info());

  // Discard puts back what's saved, and the refusal goes with it.
  await thresholds.getByRole('button', { name: 'Discard' }).click();
  await expect(confirmations()).toHaveValue(threshold);
  await expect(thresholds).not.toHaveClass(/is-failed/);
  await expect(bar).toBeHidden();

  // Saved: the card says so, and a toast.
  await confirmations().fill('4');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Settings saved');
  await expect(thresholds.locator('[data-card-saved]')).toHaveText('Saved');
  await expect(confirmations()).toHaveValue('4');
  await expect(bar).toBeHidden();
  expect(await events(page, 'mk-saved')).toEqual([{ groups: ['confirmation-thresholds'] }]);
  await captureCoverageStage(page, 'store-settings-saved', test.info());

  // Back as it was.
  await confirmations().fill(threshold);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  expect(errors).toEqual([]);
});

test('without JavaScript the save bar is always there and saves, refused or not', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(tab('payments'));
  const bar = page.locator('#save-bar');
  await expect(bar).toBeVisible();
  const field = page.locator('#card-confirmation-thresholds input[name="confirmations_required"]');
  const saved = await field.inputValue();

  await field.fill('abc');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#card-confirmation-thresholds .card-body > p.error')).toContainText('Enter a whole number');
  await expect(field).toHaveValue('abc');
  await expect(page.locator('#settings-toasts .toast-error')).toBeVisible();

  await field.fill('5');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await expect(page.locator('#card-confirmation-thresholds [data-card-saved]')).toHaveText('Saved');
  await expect(field).toHaveValue('5');
  await captureCoverageStage(page, 'store-settings-no-js', test.info());

  await field.fill(saved);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await context.close();
});

test('a store chooses its checkout layout on the Checkout tab', async ({ page, context }) => {
  await login(context);
  await page.goto(tab('checkout'));
  await expect(page.locator('.mk-tabs[aria-label="Store settings"] a[aria-current="page"]')).toHaveText('Checkout');
  const compact = page.getByRole('radio', { name: /Compact/ });
  await compact.check();
  await page.locator('#save-bar').getByRole('button', { name: /^Save/ }).click();
  await expect(page).toHaveURL(/saved=checkout-layout/);
  await expect(page.getByRole('radio', { name: /Compact/ })).toBeChecked();
});

test('the Checkout tab shows each layout as a skeleton, with Custom among them, and previews them', async ({ page, context }) => {
  await login(context);
  await page.goto(tab('checkout'));
  await expect(page.locator('.radio-card .layout-thumb')).toHaveCount(3);
  const custom = page.locator('.layout-custom');
  await expect(custom).toContainText('Custom');
  await expect(custom).toContainText('{{ your template }}');
  // No verified domain here: it's greyed out, says what it needs, and goes nowhere.
  await expect(custom).not.toContainText('Not available yet');
  await expect(custom).toContainText('Needs a verified domain');
  await expect(page.getByRole('link', { name: /^Custom/ })).toHaveCount(0);
  // Layout, the custom page, then colours: though the custom page's forms can't be
  // inside the settings form, its card is.
  expect(await page.locator('#settings-form section.settings-card').evaluateAll(cards => cards.map(card => card.id)))
    .toEqual(['card-checkout-layout', 'card-custom-page', 'card-checkout-style']);
  await captureCoverageStage(page, 'store-checkout-tab', test.info(), { group: 'custom-page', shapes: ['mobile-portrait', 'desktop'] });

  await page.locator('#card-checkout-layout').getByRole('link', { name: 'Preview every state' }).click();
  await expect(page).toHaveURL(/settings\/checkout-layout/);
  await page.getByRole('navigation', { name: 'Layouts' }).getByRole('link', { name: 'Tiny' }).click();
  await expect(page).toHaveURL(/layout=tiny/);
  const frames = page.locator('.template-preview iframe');
  await expect(frames).toHaveCount(9);
  await expect(frames.first()).toHaveAttribute('sandbox', '');
  // Frames load as they're reached: every state shows, once it has been.
  for (let at = 0; at < 9; at += 1) {
    await frames.nth(at).scrollIntoViewIfNeeded();
    await expect(page.frameLocator('.template-preview iframe').nth(at).locator('[data-part="status"]')).toBeVisible();
  }
  await captureCoverageStage(page, 'store-layout-preview', test.info(), { group: 'custom-page', shapes: ['desktop'] });
});

test('a store picks its checkout colours, starting from the theme\'s, and goes back to them', async ({ page, context }) => {
  await login(context);
  await page.goto(tab('checkout'));
  const save = () => page.locator('#save-bar').getByRole('button', { name: /^Save/ }).click();
  const accent = page.locator('#style_light_accent');
  const hex = page.getByRole('textbox', { name: 'Accent, light, as hex' });
  const back = page.getByRole('button', { name: 'Accent, light: back to the default' });

  // Nothing chosen: the theme's real colour, in the picker and as hex.
  const own = await accent.inputValue();
  expect(own).toMatch(/^#[0-9a-f]{6}$/);
  await expect(hex).toHaveValue(own);
  await expect(back).toBeHidden();
  await captureCoverageStage(page, 'store-checkout-colours', test.info(), { group: 'custom-page', shapes: ['mobile-portrait', 'desktop'] });

  // From the picker: the box follows, and it counts as a change.
  await accent.fill('#2f6b4f');
  await expect(hex).toHaveValue('#2f6b4f');
  await expect(back).toBeVisible();
  await page.locator('#save-bar').getByRole('link', { name: 'Discard changes' }).click();
  await expect(accent).toHaveValue(own);
  await expect(hex).toHaveValue(own);

  // Typed: a part isn't a colour, a whole one moves the picker, a short one is
  // read once typing is done.
  await hex.fill('#2f6');
  await expect(accent).toHaveValue(own);
  await hex.blur();
  await expect(accent).toHaveValue('#22ff66');
  await hex.fill('not a colour');
  await hex.blur();
  await expect(accent).toHaveValue('#22ff66');
  await expect(hex).toHaveJSProperty('validity.valid', false);
  await hex.fill('2F6B4F');
  await expect(accent).toHaveValue('#2f6b4f');
  await save();
  await expect(page).toHaveURL(/saved=checkout-style/);
  await expect(accent).toHaveValue('#2f6b4f');
  await expect(hex).toHaveValue('#2f6b4f');
  await captureCoverageStage(page, 'store-checkout-colours-chosen', test.info(), { group: 'custom-page', shapes: ['desktop'] });

  // Back to the theme's, saved as the theme: nothing of the store's left.
  await back.click();
  await expect(accent).toHaveValue(own);
  await expect(hex).toHaveValue(own);
  await expect(back).toBeHidden();
  await save();
  await expect(page).toHaveURL(/saved=checkout-style/);
  await expect(accent).toHaveValue(own);
  await expect(back).toBeHidden();

  // Emptying the box goes there too.
  await hex.fill('#123456');
  await hex.fill('');
  await hex.blur();
  await expect(accent).toHaveValue(own);
  await expect(hex).toHaveValue(own);
});

test('without JavaScript a store picks colours with the swatch, and a box takes them back', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(tab('checkout'));
  const accent = page.locator('#style_light_accent');
  const own = await accent.inputValue();
  await expect(page.locator('.mk-colour-hex')).toHaveCount(0);
  await expect(page.getByRole('checkbox', { name: "Go back to the theme's colours" })).toHaveCount(0);

  await accent.fill('#2f6b4f');
  await page.locator('#save-bar').getByRole('button', { name: /^Save/ }).click();
  await expect(accent).toHaveValue('#2f6b4f');
  await captureCoverageStage(page, 'store-checkout-colours-no-js', test.info(), { group: 'custom-page', shapes: ['desktop'] });

  await page.getByRole('checkbox', { name: "Go back to the theme's colours" }).check();
  await page.locator('#save-bar').getByRole('button', { name: /^Save/ }).click();
  await expect(accent).toHaveValue(own);
  await context.close();
});

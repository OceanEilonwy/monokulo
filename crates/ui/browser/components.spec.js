// The stylebook: each component's specimens, as crates/ui renders them,
// working with and without JavaScript. The Solid translation is checked
// against the same specimens in solid-equivalence.spec.js.
import { test, expect } from '@playwright/test';

const noScript = async (browser, url = 'http://127.0.0.1:4179') => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(url);
  return { context, page };
};

test('every component has a section with its props and a specimen', async ({ page }) => {
  await page.goto('/');
  const links = await page.locator('.book-nav a').evaluateAll(all => all.map(a => a.getAttribute('href')));
  expect(links.length).toBeGreaterThan(40);
  for (const href of links) {
    const section = page.locator(href);
    await expect(section.locator(':scope > h2')).toHaveCount(1);
    await expect(section.locator(':scope table.book-props')).toHaveCount(1);
  }
  // Every id on the page is unique, every label names something.
  const duplicates = await page.evaluate(() => {
    const seen = new Map();
    for (const el of document.querySelectorAll('[id]')) seen.set(el.id, (seen.get(el.id) || 0) + 1);
    return [...seen].filter(([, n]) => n > 1).map(([id]) => id);
  });
  expect(duplicates).toEqual([]);
  const unlabelled = await page.evaluate(() => [...document.querySelectorAll('label[for]')]
    .filter(label => !document.getElementById(label.htmlFor)).map(label => label.htmlFor));
  expect(unlabelled).toEqual([]);
});

test('the select supports the keyboard and stays the form field', async ({ page }) => {
  await page.goto('/#component-select');
  const select = page.locator('#book-currency');
  const combobox = page.getByRole('combobox', { name: 'Base currency' });
  const before = await select.inputValue();
  await combobox.focus();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(select).not.toHaveValue(before);
  const picked = await select.inputValue();
  await select.evaluate(el => { el.value = el.options[0].value; el.dispatchEvent(new Event('change', { bubbles: true })); });
  await expect(combobox).toContainText(await select.evaluate(el => el.options[0].dataset.label || el.options[0].text));
  expect(picked).not.toBe(await select.inputValue());
});

test('without JavaScript every specimen is native HTML that works', async ({ browser }) => {
  const { context, page } = await noScript(browser);
  await expect(page.locator('#book-currency')).toBeVisible();
  await page.locator('#book-currency').selectOption({ index: 1 });
  const disclosure = page.locator('#component-disclosure-1 .book-example details');
  await disclosure.locator('summary').click();
  await expect(disclosure).toHaveAttribute('open', '');
  const box = page.locator('#component-checkbox-1 .book-example input[type=checkbox]');
  await box.check();
  await expect(box).toBeChecked();
  // Tabs are links to their panels, every panel shown.
  await expect(page.locator('#component-tabs section')).toHaveCount(3);
  for (const panel of await page.locator('#component-tabs section').all()) await expect(panel).toBeVisible();
  // The settings form posts as a plain form.
  await page.locator('#book-settings-name-input').fill('Plain store');
  await page.locator('#book-form-save-bar').getByRole('button', { name: 'Save' }).click();
  await expect(page).toHaveURL(/name=Plain\+store/);
  await context.close();
});

test('copy takes the whole current value; a refused clipboard copies the selection, or leaves it to copy by hand', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/#component-copy-value');
  const value = page.locator('#component-copy-value-1 [data-copy-value]');
  const copy = page.getByRole('button', { name: 'Copy payment reference' });
  const full = await value.textContent();
  await copy.click();
  await expect(page.locator('#component-copy-value-1 .mk-copy-status')).toHaveText('Copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(full);
  await value.evaluate(el => { el.textContent = 'updated value <literal> & complete'; });
  await copy.click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('updated value <literal> & complete');
  // A refused clipboard: the value selected and copied the old way...
  await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { value: { writeText: () => Promise.reject(new Error('denied')) }, configurable: true }));
  await copy.click();
  await expect(page.locator('#component-copy-value-1 .mk-copy-status')).toHaveText('Copied');
  expect(await page.evaluate(() => getSelection().toString())).toBe('updated value <literal> & complete');
  // ...or, where that's refused too, left selected to copy by hand.
  await page.evaluate(() => { document.execCommand = () => false; });
  await copy.click();
  await expect(page.locator('#component-copy-value-1 .mk-copy-status')).toContainText('copy command');
  expect(await page.evaluate(() => getSelection().toString())).toBe('updated value <literal> & complete');
  await expect(value).toBeFocused();
  // Shortened forms still copy the whole value.
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.reload();
  for (const [n, name] of [[2, 'Copy middle example'], [3, 'Copy segmented example']]) {
    const whole = await page.locator(`#component-copy-value-${n} [data-copy-value]`).textContent();
    await expect(page.locator(`#component-copy-value-${n} .mk-copy-preview`)).toContainText('…');
    await page.getByRole('button', { name }).click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(whole);
  }
  // A copy block copies its lines without their notes.
  await page.getByRole('button', { name: 'Copy the key commands' }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('umask 077\nopenssl rand -hex 32 > monokulo.key');
  await expect(page.getByRole('button', { name: 'Copy the key commands' })).toHaveText('Copied');
});

test('tabs, the account menu and a dialog take the keyboard', async ({ page }) => {
  await page.goto('/');
  const tabs = page.getByRole('tablist', { name: 'Example POS views' });
  // Tabs with logos in their labels keep working, and tell the page.
  const told = page.evaluate(() => new Promise(resolve => document.addEventListener('mk-tab', e => resolve(e.detail.id), { once: true })));
  await tabs.getByRole('tab', { name: 'History' }).click();
  expect(await told).toBe('example-history');
  await tabs.getByRole('tab', { name: 'Charge' }).click();
  await tabs.getByRole('tab', { name: 'Charge' }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(tabs.getByRole('tab', { name: 'History' })).toBeFocused();
  await expect(page.getByRole('tabpanel', { name: 'History' })).toBeVisible();
  await expect(page.locator('#example-charge')).toBeHidden();
  await page.keyboard.press('End');
  await expect(tabs.getByRole('tab', { name: 'Terminal' })).toHaveAttribute('aria-selected', 'true');

  const menu = page.locator('#component-menu details.acct');
  await menu.locator('summary').click();
  await menu.getByRole('link').first().focus();
  await page.keyboard.press('Escape');
  await expect(menu).not.toHaveAttribute('open', '');
  await expect(menu.locator('summary')).toBeFocused();
  await menu.locator('summary').click();
  await page.locator('#component-menu > h2').click();
  await expect(menu).not.toHaveAttribute('open', '');

  // A link naming the dialog opens it; Escape closes it and focus goes back.
  await page.evaluate(() => {
    const opener = document.createElement('a');
    opener.href = '#component-dialog-page';
    opener.textContent = 'Review retirement';
    opener.dataset.opensDialog = 'book-dialog';
    document.querySelector('#component-dialog h2').after(opener);
  });
  await page.getByRole('link', { name: 'Review retirement' }).click();
  const dialog = page.getByRole('dialog', { name: 'Retire this wallet?' });
  await expect(dialog).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('link', { name: 'Review retirement' })).toBeFocused();
  await page.getByRole('link', { name: 'Review retirement' }).click();
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
});

test('a settings form marks what changed, discards it, and saves as a form', async ({ page }) => {
  await page.goto('/#component-settings-form');
  const bar = page.getByRole('region', { name: "Example form's save changes" });
  const input = page.locator('#book-settings-name-input');
  await expect(bar).toBeHidden();
  await input.fill('Changed store');
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('1 unsaved change');
  await expect(page.locator('#card-book-details')).toHaveClass(/is-dirty/);
  await expect(page.locator('#card-book-details .changed-mark')).toBeVisible();
  await page.locator('#card-book-details [data-card-discard]').click();
  await expect(input).toHaveValue('Corner store');
  await expect(bar).toBeHidden();
  await input.fill('Saved example');
  await bar.getByRole('button', { name: 'Save' }).click();
  await expect(page).toHaveURL(/name=Saved\+example/);
});

test('a sortable list reorders by drag and by the arrow keys, and says so', async ({ page }) => {
  // Without the slide, so where each row is doesn't depend on timing.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/#component-sortable-list');
  const list = page.locator('#component-sortable-list-1 .mk-sortable');
  const places = () => list.locator(':scope > li summary').allTextContents();
  await list.evaluate(el => { el.events = []; el.addEventListener('mk-reordered', () => el.events.push('reordered')); });
  const first = list.locator('[data-sortable-handle]').first();
  await first.focus();
  await page.keyboard.press('ArrowDown');
  expect((await places())[1]).toContain('Primary');
  await expect(list.locator('[data-sortable-handle]').nth(1)).toBeFocused();
  // The fixed row stays last, whatever moves.
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  expect((await places())[3]).toContain('Add a node');
  await list.scrollIntoViewIfNeeded();
  const box = await list.locator('[data-sortable-handle]').nth(2).boundingBox();
  const target = await list.locator(':scope > li').first().boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, target.y + 2, { steps: 8 });
  await page.mouse.up();
  expect((await places())[0]).toContain('Primary');
  expect(await list.evaluate(el => el.events.length)).toBeGreaterThan(2);
});

test('a help tip opens over the page and closes on Escape', async ({ page }) => {
  await page.goto('/#component-help-tip');
  const tip = page.locator('#component-help-tip-1 details.help-tip');
  await tip.locator('summary').click();
  await expect(tip.locator('.help-tip-body')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(tip).not.toHaveAttribute('open', '');
  await expect(tip.locator('summary')).toBeFocused();
});

test('a grouped value copies the whole value, says so for a while, and stays whole on screen', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/#component-grouped-value');
  const example = page.locator('#component-grouped-value-1');
  const shown = example.locator('.mk-grouped-value');
  const before = await shown.innerHTML();
  const whole = await example.locator('.visually-hidden').first().textContent();
  // The groups hold the value with nothing between them: a selection of
  // them is the value.
  expect(await shown.textContent()).toBe(whole);
  await expect(shown.locator('span')).toHaveCount(16);
  await example.getByRole('button', { name: 'Copy address' }).click();
  await expect(example.getByRole('button', { name: 'Copied' })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(whole);
  expect(await shown.innerHTML()).toBe(before);
  await expect(example.getByRole('button', { name: 'Copy address' })).toBeVisible({ timeout: 6000 });
  // A value no longer to be used has nothing to copy it with.
  await expect(page.locator('#component-grouped-value-2 button')).toHaveCount(0);
});

test('rows mark removal at once and undo it, and the blank row waits behind Add', async ({ page }) => {
  await page.goto('/#component-rows-editor');
  const editor = page.locator('#component-rows-editor-1');
  const box = editor.locator('input[data-rows-remove][name="tier_1"]');
  const remove = editor.getByRole('button', { name: 'Remove the 500 EUR tier' });
  await expect(box).toBeHidden();
  await expect(editor.locator('.mk-rows-new')).toBeHidden();
  await remove.click();
  await expect(box).toBeChecked();
  await expect(remove).toHaveAttribute('aria-pressed', 'true');
  await expect(remove).toContainText('Undo');
  await remove.click();
  await expect(box).not.toBeChecked();
  await editor.getByRole('button', { name: 'Add a tier' }).click();
  await expect(editor.locator('.mk-rows-new')).toBeVisible();
  await expect(editor.getByRole('textbox', { name: "New tier's amount, EUR" })).toBeFocused();
  await expect(editor.getByRole('button', { name: 'Add a tier' })).toBeHidden();
});

test('without JavaScript rows remove with a checkbox, the blank row is there, and a countdown shows the server\'s time', async ({ browser }) => {
  const { context, page } = await noScript(browser);
  const editor = page.locator('#component-rows-editor-1');
  await expect(editor.locator('.mk-rows-toggle')).toHaveCount(2);
  await expect(editor.locator('.mk-rows-toggle').first()).toBeHidden();
  const box = editor.getByRole('checkbox', { name: /Remove the 500 EUR tier/ });
  await box.check();
  await expect(box).toBeChecked();
  await expect(editor.locator('.mk-rows-new')).toBeVisible();
  await expect(page.locator('#component-countdown-1')).toContainText('29m left to pay');
  await context.close();
});

test('a countdown counts on from the seconds it was given, and turns as time runs low', async ({ page }) => {
  await page.clock.install();
  await page.goto('/#component-countdown');
  const first = page.locator('#component-countdown-1 .mk-countdown');
  await expect(first.locator('.mk-countdown-clock')).toHaveText('29:14');
  await expect(first).toHaveAttribute('data-tone', 'normal');
  await page.clock.runFor(3000);
  await expect(first.locator('.mk-countdown-clock')).toHaveText('29:11');
  const low = page.locator('#component-countdown-2 .mk-countdown');
  await expect(low).toHaveAttribute('data-tone', 'soon');
  await page.clock.runFor(235_000);
  await expect(low).toHaveAttribute('data-tone', 'urgent');
  await expect(low.getByRole('status')).toHaveText('1 minute left.');
  const ended = low.evaluate(el => new Promise(resolve => el.addEventListener('mk-countdown-end', () => resolve(true), { once: true })));
  await page.clock.runFor(60_000);
  expect(await ended).toBe(true);
  await expect(low.getByRole('status')).toHaveText('Time is up.');
  await expect(low.locator('progress')).toHaveAttribute('value', '0');
});

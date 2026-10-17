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

test('the development server serves the same styled specimens', async ({ browser }) => {
  const { context, page } = await noScript(browser, 'http://127.0.0.1:4180');
  await expect(page.locator('#book-currency')).toBeVisible();
  expect(await page.locator('.mk-btn').first().evaluate(el => getComputedStyle(el).borderRadius)).not.toBe('0px');
  await context.close();
});

for (const theme of ['light', 'dark']) {
  test(`${theme}: a phone's width fits, and states read`, async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 780 });
    await page.goto('/');
    await page.locator('#book-theme').selectOption(theme, { force: true });
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await expect(page.locator('#component-button button[disabled]').first()).toBeDisabled();
    const input = page.locator('#book-email');
    await expect(input).toHaveAccessibleDescription('Enter a complete email address.');
    await input.focus();
    expect(await input.evaluate(el => getComputedStyle(el).outlineStyle)).toBe('solid');
    await page.screenshot({ path: `test-results/stylebook-${theme}.png`, fullPage: true });
  });
}

test('the wizard strip counts completed tasks and keeps its detail on a phone', async ({ page }) => {
  await page.goto('/');
  const wallet = page.locator('#component-wizard-steps-1 [aria-current="step"]');
  await expect(wallet).toHaveText('WalletBack up · step 2 of 3');
  await expect(wallet.locator('progress')).toHaveAttribute('value', '1');
  await page.setViewportSize({ width: 320, height: 740 });
  await expect(wallet.locator('small')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
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
});

test('tabs, the account menu and a dialog take the keyboard', async ({ page }) => {
  await page.goto('/');
  const tabs = page.getByRole('tablist', { name: 'Example POS views' });
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

test('buttons share a height, and messages and toasts a border all round on a solid ground', async ({ page }) => {
  await page.goto('/');
  const heights = await page.locator('#component-button-1 .mk-btn, #component-button-2 .mk-btn, #component-button-3 .mk-btn, #component-button-4 .mk-btn')
    .evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().height));
  expect(Math.max(...heights) - Math.min(...heights)).toBeLessThan(1);
  const feedback = await page.locator('#component-toast .toast, #component-alert :is(.error, .warning, .success, .notice)').evaluateAll(nodes => nodes.map(node => {
    const css = getComputedStyle(node);
    return { toast: node.matches('.toast'), borders: [css.borderTopWidth, css.borderRightWidth, css.borderBottomWidth, css.borderLeftWidth], background: css.backgroundColor };
  }));
  expect(feedback.length).toBeGreaterThan(4);
  for (const item of feedback) {
    expect(new Set(item.borders).size).toBe(1);
    expect(item.background).not.toMatch(/rgba\([^)]*,\s*0\)|transparent/);
  }
});

test('tags are pills; an order state has its colours; Current is plain words', async ({ page }) => {
  await page.goto('/');
  const tag = page.locator('#component-tag .tag.state-pending').first();
  await expect(tag).toHaveCSS('border-radius', '999px');
  expect(await tag.evaluate(el => getComputedStyle(el).backgroundColor)).not.toBe(
    await page.locator('#component-tag .tag:not([class*="state-"]):not([class*="tag-"])').first().evaluate(el => getComputedStyle(el).backgroundColor));
  // The Select's wallet specimen.
  const selector = page.locator('#component-select-2 .mk-select');
  const current = selector.locator('.mk-button .mk-current');
  await expect(current).toHaveText('Current');
  await expect(current).toHaveCSS('border-top-width', '0px');
  await expect(current).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await expect(selector.locator('.mk-button .tag-network')).toHaveCount(1);
  await expect(selector.locator('.mk-button .tag')).toHaveCount(0);
  await selector.locator('.mk-button').click();
  await expect(selector.getByRole('option', { name: /Copper Heron/ }).locator('.mk-current')).toHaveText('Current');
});

test('a table becomes cards on a phone, and facts stack', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.goto('/#component-table');
  await expect(page.locator('#component-table-1 thead')).toBeHidden();
  expect(await page.locator('#component-table-1 tr').first().evaluate(el => getComputedStyle(el).display)).toBe('grid');
  expect(await page.locator('#component-table-2 th').first().evaluate(el => getComputedStyle(el).display)).toBe('block');
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

test('a copy block copies its lines without their notes', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/#component-copy-block');
  await page.getByRole('button', { name: 'Copy the key commands' }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('umask 077\nopenssl rand -hex 32 > monokulo.key');
  await expect(page.getByRole('button', { name: 'Copy the key commands' })).toHaveText('Copied');
});

test('tabs with logos in their labels keep working, and tell the page', async ({ page }) => {
  await page.goto('/');
  const tabs = page.getByRole('tablist', { name: 'Example POS views' });
  const told = page.evaluate(() => new Promise(resolve => document.addEventListener('mk-tab', e => resolve(e.detail.id), { once: true })));
  await tabs.getByRole('tab', { name: 'History' }).click();
  expect(await told).toBe('example-history');
});

// A part-done step's dot is filled on its right half with one straight
// edge. Its 2.13em is a fraction of a pixel, and where the dot lands in
// a pixel moves with the page's width; at some, a conic-gradient's hard
// stop drew the top and bottom halves of the edge a device pixel apart, a
// notch down the middle. Phone widths a pixel apart cover where it lands.
for (const theme of ['light', 'dark']) {
  for (const scale of [1, 1.5, 2]) {
    test(`${theme}, ${scale}x: a part-done step is half filled with one straight edge on a phone`, async ({ browser }) => {
      const context = await browser.newContext({ viewport: { width: 320, height: 740 }, deviceScaleFactor: scale, colorScheme: theme });
      const page = await context.newPage();
      await page.goto('/');
      const dot = page.locator('.step-part .mk-track-dot').first();
      for (let width = 320; width < 332; width++) {
        await page.setViewportSize({ width, height: 740 });
        await dot.scrollIntoViewIfNeeded();
        const png = (await dot.screenshot({ animations: 'disabled' })).toString('base64');
        // Read the pixels back through a canvas: where each row turns from
        // the left half's colour to the right's.
        const { edges, left, right } = await page.evaluate(async png => {
        const image = new Image();
        image.src = `data:image/png;base64,${png}`;
        await image.decode();
        const canvas = Object.assign(document.createElement('canvas'), { width: image.width, height: image.height });
        const ctx = canvas.getContext('2d');
        ctx.drawImage(image, 0, 0);
        const { data, width, height } = ctx.getImageData(0, 0, image.width, image.height);
        const at = (x, y) => [...data.slice((y * width + x) * 4, (y * width + x) * 4 + 3)];
        const mid = Math.round(height / 2);
        const left = at(Math.round(width * 0.3), mid), right = at(Math.round(width * 0.7), mid);
        const near = (a, b) => a.reduce((sum, v, i) => sum + Math.abs(v - b[i]), 0);
        const edges = [];
        for (let y = Math.round(height * 0.25); y < Math.round(height * 0.75); y++) {
          let x = Math.round(width * 0.3);
          while (x < width * 0.7 && near(at(x, y), left) <= near(at(x, y), right)) x++;
          edges.push(x);
        }
        return { edges, left, right };
      }, png);
        expect(left, `${width}px`).not.toEqual(right);
        expect(new Set(edges).size, `${width}px: edge columns ${edges}`).toBe(1);
      }
      await context.close();
    });
  }
}

import { test, expect } from '@playwright/test';

test('enhanced select supports keyboard, programmatic changes and form reset', async ({ page }) => {
  await page.goto('/');
  const select = page.locator('#currency');
  const combobox = page.getByRole('combobox', { name: 'Currency', exact: true });
  await expect(combobox).toContainText('Australian dollar');
  await combobox.focus();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(select).toHaveValue('EUR');
  await page.getByRole('button', { name: 'Reset', exact: true }).click();
  await expect(select).toHaveValue('AUD');
  await expect(combobox).toContainText('Australian dollar');
  await page.getByRole('button', { name: 'Change with JavaScript' }).click();
  await expect(select).toHaveValue('EUR');
  await expect(combobox).toContainText('Euro');
  await page.getByRole('button', { name: 'Submit normally' }).click();
  await expect(page).toHaveURL(/currency=EUR/);
});

test('native forms and disclosures work with JavaScript disabled', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:4179');
  await expect(page.locator('#currency')).toBeVisible();
  await page.locator('#currency').selectOption('EUR');
  await page.getByRole('button', { name: 'Submit normally' }).click();
  await expect(page).toHaveURL(/currency=EUR/);
  await page.getByText('Refund information', { exact: true }).click();
  await expect(page.getByText('Contact the merchant if a refund is needed.', { exact: true })).toBeVisible();
  await page.getByLabel('Enable notifications').check();
  await expect(page.getByLabel('Enable notifications')).toBeChecked();
  await page.getByLabel('Display name').fill('Example');
  await page.getByRole('button', { name: 'Submit example' }).click();
  await expect(page).toHaveURL(/display-name=Example/);
  await context.close();
});

for (const theme of ['light', 'dark']) {
  test(`${theme}: narrow layout and shared states remain usable`, async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 780 });
    await page.goto('/');
    await page.locator('#book-theme').selectOption(theme, { force: true });
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await expect(page.locator('#component-button-8 button')).toBeDisabled();
    const input = page.locator('#book-email');
    await expect(input).toHaveAccessibleDescription('Enter a complete email address.');
    await input.focus();
    expect(await input.evaluate(el => getComputedStyle(el).outlineStyle)).toBe('solid');
    await page.screenshot({ path: `test-results/stylebook-${theme}.png`, fullPage: true });
  });
}


test('development server delivers styled native examples without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:4180');
  await expect(page.locator('#currency')).toBeVisible();
  expect(await page.locator('.mk-grid').first().evaluate(el => getComputedStyle(el).display)).toBe('grid');
  expect(await page.locator('.mk-btn').first().evaluate(el => getComputedStyle(el).borderRadius)).not.toBe('0px');
  await context.close();
});

test('breadcrumbs, back links and action cards retain native behaviour without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:4179');
  const trail = page.getByRole('navigation', { name: 'Example breadcrumb' });
  await expect(trail.locator('[aria-current="page"]')).toHaveText('Orders');
  await expect(trail.getByRole('link').nth(1)).toHaveAccessibleName('A store with a deliberately long name to review truncation');
  await page.getByRole('link', { name: 'Back to store details' }).focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#forms$/);
  await page.getByRole('link', { name: 'Connect WooCommerce' }).click();
  await expect(page).toHaveURL(/#compositions$/);
  await expect(page.getByRole('button', { name: 'Connect a device' })).toBeDisabled();
  await page.getByRole('button', { name: 'Bring your own wallet', exact: true }).click();
  await expect(page).toHaveURL(/wallet-kind=existing#action-cards$/);
  await context.close();
});

test('wizard details stay in the top-level segment at desktop and mobile widths', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto('/');
  const wallet = page.locator('#component-progress-steps-1 [aria-current="step"]');
  await expect(wallet).toHaveText('WalletBack up · step 2 of 3');
  await expect(page.locator('#component-progress-steps .mk-substeps, #component-progress-steps .mk-step-summary')).toHaveCount(0);
  await page.setViewportSize({ width: 320, height: 740 });
  await expect(wallet.locator('small')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const cards = await page.locator('#action-cards .mk-action-card').evaluateAll(elements => elements.map(el => {
    const box = el.getBoundingClientRect();
    return { x: box.x, width: box.width, y: box.y, height: box.height };
  }));
  expect(cards[1].y).toBeGreaterThanOrEqual(cards[0].y + cards[0].height);
  expect(cards.every(card => card.x >= 0 && card.x + card.width <= 320)).toBe(true);
});

test('copy uses the full current value, and permission denial offers manual selection', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/#component-copy-value');
  const value = page.locator('.mk-copy [data-copy-value]').first();
  const full = await value.textContent();
  await page.getByRole('button', { name: 'Copy payment reference' }).click();
  await expect(page.locator('.mk-copy-status').first()).toHaveText('Copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(full);
  await value.evaluate(el => { el.textContent = 'updated value <literal> & complete'; });
  await page.getByRole('button', { name: 'Copy payment reference' }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('updated value <literal> & complete');
  await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { value: { writeText: () => Promise.reject(new Error('denied')) }, configurable: true }));
  await page.getByRole('button', { name: 'Copy payment reference' }).click();
  await expect(page.locator('.mk-copy-status').first()).toContainText('copy command');
  expect(await page.evaluate(() => getSelection().toString())).toBe('updated value <literal> & complete');
  await expect(value).toBeFocused();
});

test('tabs, account menu and dialog support keyboard interaction', async ({ page }) => {
  await page.goto('/');
  const tabs = page.getByRole('tablist', { name: 'Example POS views' });
  await tabs.getByRole('tab', { name: 'Charge' }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(tabs.getByRole('tab', { name: 'History' })).toBeFocused();
  await expect(page.getByRole('tabpanel', { name: 'History' })).toBeVisible();
  await expect(page.locator('#example-charge')).toBeHidden();
  await page.keyboard.press('End');
  await expect(tabs.getByRole('tab', { name: 'Terminal' })).toHaveAttribute('aria-selected', 'true');
  const menu = page.locator('.mk-menu');
  await menu.locator('summary').click();
  await menu.getByRole('link', { name: 'Account settings' }).focus();
  await page.keyboard.press('Escape');
  await expect(menu).not.toHaveAttribute('open', '');
  await expect(menu.locator('summary')).toBeFocused();
  await menu.locator('summary').click();
  await page.locator('#menus > h2').click();
  await expect(menu).not.toHaveAttribute('open', '');
  await page.getByRole('link', { name: 'Review retirement' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toBeHidden();
  await expect(page.getByRole('link', { name: 'Review retirement' })).toBeFocused();
});

test('real settings enhancement marks changes, discards and submits natively', async ({ page }) => {
  await page.goto('/#page-forms');
  const bar = page.getByRole('region', { name: 'Example save changes' });
  await expect(bar).toBeHidden();
  await page.locator('#example-store-name').fill('Changed store');
  await expect(bar).toBeVisible();
  await expect(page.locator('#card-example')).toHaveClass(/is-dirty/);
  await expect(page.locator('#card-example .changed-mark').first()).toBeVisible();
  await page.locator('#card-example [data-card-discard]').click();
  await expect(page.locator('#example-store-name')).toHaveValue('Corner store');
  await expect(bar).toBeHidden();
  await page.locator('#example-store-name').fill('Saved example');
  await page.getByRole('button', { name: 'Save example settings' }).click();
  await expect(page).toHaveURL(/store-name=Saved\+example&.*#page-forms/);
});

test('new patterns retain content and actions without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:4179');
  await expect(page.locator('#example-charge')).toBeVisible();
  await expect(page.locator('#example-history')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Copy payment reference' })).toHaveCount(0);
  const value = page.locator('[data-copy-value]').first();
  await value.focus();
  expect(await value.evaluate(el => getComputedStyle(el).whiteSpace)).toBe('normal');
  await page.getByLabel('Example account menu').click();
  await expect(page.locator('.mk-menu').getByRole('link', { name: 'Account settings' })).toBeVisible();
  await page.getByRole('link', { name: 'Review retirement' }).click();
  await expect(page).toHaveURL(/#dialog-fallback$/);
  await expect(page.getByRole('region', { name: 'Example save changes' })).toBeVisible();
  await page.locator('#example-store-name').fill('Native settings');
  await page.getByRole('button', { name: 'Save example settings' }).click();
  await expect(page).toHaveURL(/store-name=Native\+settings/);
  await expect(page.locator('#qr svg')).toBeVisible();
  await context.close();
});

test('spinner honours reduced motion and arrows sit outside underline decoration', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  expect(await page.locator('.mk-spinner').first().evaluate(el => getComputedStyle(el).animationName)).toBe('none');
  const arrow = await page.locator('.mk-back-link').first().evaluate(el => {
    const style = getComputedStyle(el, '::before');
    return { position: style.position, decoration: style.textDecorationLine };
  });
  expect(arrow).toEqual({ position: 'absolute', decoration: 'none' });
});

test('buttons align, breadcrumbs truncate on one line, and feedback has uniform opaque borders', async ({ page }) => {
  await page.goto('/');
  const heights = await page.locator('#icon-buttons .mk-btn').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().height));
  expect(Math.max(...heights) - Math.min(...heights)).toBeLessThan(1);
  const feedback = await page.locator('#component-toast .toast, #component-alert .mk-alert').evaluateAll(nodes => nodes.map(node => {
    const css = getComputedStyle(node);
    return { borders: [css.borderTopWidth, css.borderRightWidth, css.borderBottomWidth, css.borderLeftWidth], background: css.backgroundColor };
  }));
  for (const item of feedback) {
    expect(new Set(item.borders).size).toBe(1);
    expect(item.background).not.toMatch(/rgba\([^)]*,\s*0\)|transparent/);
  }
  await page.setViewportSize({ width: 320, height: 740 });
  const trail = page.getByRole('navigation', { name: 'Example breadcrumb' });
  const positions = await trail.locator('li').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().y));
  expect(Math.max(...positions) - Math.min(...positions)).toBeLessThan(1);
  expect(await trail.getByRole('link').nth(1).evaluate(node => node.scrollWidth > node.clientWidth)).toBe(true);
  await expect(trail.getByRole('link').nth(1)).toHaveAccessibleName('A store with a deliberately long name to review truncation');
});

test('middle and segmented presentation always copy the complete current value', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/#component-copy-value');
  // The CopyValue specimens: 2 is middle, 3 is segments.
  for (const n of [2, 3]) {
    const host = page.locator(`#component-copy-value-${n} .mk-copy`);
    const original = await host.locator('[data-copy-value]').textContent();
    await expect(host.locator('.mk-copy-preview')).toHaveCount(1);
    expect(await host.locator('.mk-copy-preview').textContent()).toContain('…');
    await host.getByRole('button').click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(original);
    await host.locator('.mk-copy-preview').click();
    await expect(host.locator('[data-copy-value]')).toBeFocused();
    expect(await page.evaluate(() => getSelection().toString())).toBe(original);
    await expect(host.locator('.mk-copy-preview')).toBeHidden();
    await host.locator('[data-copy-value]').evaluate(node => { node.textContent = 'short'; });
    await expect(host.locator('.mk-copy-preview')).toHaveCount(0);
  }
});

test('full-page settings keep save actions in the footer with and without scripts', async ({ browser }) => {
  for (const javaScriptEnabled of [true, false]) {
    const context = await browser.newContext({ javaScriptEnabled, viewport: { width: 320, height: 740 } });
    const page = await context.newPage();
    await page.goto('http://127.0.0.1:4179/form-page.html');
    await page.locator('#example-store-name').fill('Footer check');
    const save = page.getByRole('button', { name: 'Save example settings' });
    await expect(save).toBeInViewport();
    const before = await page.locator('footer').boundingBox();
    await page.locator('.mk-page-body').evaluate(node => { node.scrollTop = node.scrollHeight; });
    await expect(save).toBeInViewport();
    expect((await page.locator('footer').boundingBox()).y).toBe(before.y);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await context.close();
  }
});

test('pagination and QR destinations are real native links', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('http://127.0.0.1:4179');
  await page.getByRole('navigation', { name: 'Example order pagination' }).getByRole('link', { name: 'Next page', exact: true }).click();
  await expect(page).toHaveURL(/page=3#pagination/);
  await expect(page.locator('#qr > .book-example .mk-qr > a')).toHaveAttribute('href', 'https://example.com/');
  await context.close();
});

test('catalogue contracts: IDs, labels, SVG references and reduced motion', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  const issues = await page.evaluate(() => {
    const errors = [];
    const seen = new Set();
    document.querySelectorAll('[id]').forEach(node => {
      if (seen.has(node.id)) errors.push(`Duplicate ID ${node.id}`);
      seen.add(node.id);
    });
    document.querySelectorAll('label[for]').forEach(node => {
      if (!document.getElementById(node.htmlFor)) errors.push(`Missing label target ${node.htmlFor}`);
    });
    document.querySelectorAll('[aria-labelledby], [aria-describedby], [aria-controls]').forEach(node => {
      for (const attr of ['aria-labelledby', 'aria-describedby', 'aria-controls']) {
        for (const id of (node.getAttribute(attr) || '').split(/\s+/).filter(Boolean)) {
          if (!document.getElementById(id)) errors.push(`Missing ${attr} target ${id}`);
        }
      }
    });
    document.querySelectorAll('use[href^="#"]').forEach(node => {
      if (!document.getElementById(node.getAttribute('href').slice(1))) errors.push('Missing SVG symbol');
    });
    return errors;
  });
  expect(issues).toEqual([]);
  await expect(page.locator('#brand-loading .logo-glow').first()).toBeHidden();
  await expect(page.locator('#pos-status .pos-icon')).toHaveCount(10);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('selected progress strip uses completed tasks without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('/');
  // The progress strip's first four specimens: setup at each stage.
  for (const [n, value, max] of [[1, 1, 3], [2, 1, 2], [3, 2, 3], [4, 1, 1]]) {
    const strip = page.locator(`#component-progress-steps-${n} ol`);
    await expect(strip).toHaveAccessibleName('Setup progress');
    const progress = strip.locator('progress').nth(1);
    await expect(progress).toHaveAttribute('value', String(value));
    await expect(progress).toHaveAttribute('max', String(max));
    expect(await progress.evaluate(el => el.position)).toBeCloseTo(value / max);
    await expect(progress).toBeVisible();
  }
  await page.setViewportSize({ width: 320, height: 740 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await context.close();
});

test('feedback icons share a size and text alignment; empty tables have breathing room', async ({ page }) => {
  await page.goto('/');
  const metrics = await page.locator('#component-toast .toast').evaluateAll(toasts => toasts.map(toast => {
    const box = toast.getBoundingClientRect();
    const icon = toast.querySelector('.toast-icon').getBoundingClientRect();
    const text = toast.querySelector('.toast-text').getBoundingClientRect();
    return { iconWidth: icon.width, iconHeight: icon.height, textOffset: text.x - box.x };
  }));
  expect(metrics).toHaveLength(4);
  expect(metrics.every(metric => metric.iconWidth === 24 && metric.iconHeight === 24)).toBe(true);
  expect(new Set(metrics.map(metric => metric.textOffset)).size).toBe(1);
  const empty = page.locator('#data-display .mk-empty-state');
  expect((await empty.boundingBox()).height).toBeGreaterThanOrEqual(240);
  const gaps = await empty.evaluate(el => {
    const [title, description, action] = [...el.children].map(child => child.getBoundingClientRect());
    return [description.top - title.bottom, action.top - description.bottom];
  });
  expect(gaps.every(gap => gap >= 12)).toBe(true);
  await expect(page.locator('#step-options')).toHaveCount(0);
});

test('page forms track edits and fit narrow screens with native fallbacks', async ({ browser }) => {
  for (const javaScriptEnabled of [true, false]) {
    const context = await browser.newContext({ javaScriptEnabled, viewport: { width: 320, height: 740 } });
    const page = await context.newPage();
    for (const layout of ['grouped']) {
      await page.goto('/form-' + layout + '.html');
      const input = page.locator('#' + layout + '-name');
      await expect(input).toBeVisible();
      await input.fill('Updated shop');
      await expect(page.getByRole('button', { name: 'Save settings' })).toBeVisible();
      if (javaScriptEnabled) {
        await expect(page.locator('.settings-card').first()).toHaveClass(/is-dirty/);
        await page.locator('.settings-card').first().getByRole('button', { name: 'Discard', exact: true }).click();
        await expect(input).toHaveValue('Corner store');
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const footer = await page.locator('.mk-page-footer').boundingBox();
      expect(footer.y + footer.height).toBeLessThanOrEqual(741);
    }
    await context.close();
  }
});

test('grouped forms do not reflow when editing, changing counts or discarding', async ({ page }) => {
  for (const width of [1100, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/form-grouped.html');
    await page.locator('#grouped-name').focus();
    const geometry = () => page.evaluate(() => {
      const body = document.querySelector('.mk-page-body');
      const origin = body.getBoundingClientRect();
      const boxes = [...document.querySelectorAll('.card-head h3, .mk-setting-group, .setting-label, .mk-setting-control, .mk-page-footer')].map(el => {
        const r = el.getBoundingClientRect();
        return [r.x, el.closest('.mk-page-body') ? r.y - origin.y + body.scrollTop : r.y, r.width, r.height];
      });
      return { boxes, scrollHeight: body.scrollHeight, height: body.clientHeight };
    });
    const before = await geometry();
    await page.locator('#grouped-name').fill('Edited name');
    await expect(page.locator('#card-grouped-details [data-unsaved]')).toHaveText('1 unsaved');
    expect(await geometry()).toEqual(before);
    await page.locator('#grouped-site').fill('https://changed.example');
    await expect(page.locator('#card-grouped-details [data-unsaved]')).toHaveText('2 unsaved');
    expect(await geometry()).toEqual(before);
    await page.locator('#card-grouped-details [data-card-discard]').click();
    await expect(page.locator('#grouped-name')).toHaveValue('Corner store');
    await expect(page.locator('#card-grouped-details [data-card-discard]')).toBeHidden();
    await expect(page.locator('#card-grouped-details .changed-mark').first()).toBeHidden();
    expect(await geometry()).toEqual(before);
  }
});

test('editing a setting highlights only its wrapper and section title', async ({ page }) => {
  await page.goto('/form-grouped.html');
  const section = page.locator('#card-grouped-payments');
  const groups = section.locator('.mk-setting-group');
  const title = section.locator('h3');
  const border = el => el.evaluate(node => getComputedStyle(node).borderTopColor);
  const color = el => el.evaluate(node => getComputedStyle(node).color);
  const normalBorder = await border(groups.first());
  const normalTitle = await color(title);
  await page.locator('#grouped-terminal').uncheck();
  expect(await border(groups.first())).toBe(normalBorder);
  expect(await border(groups.nth(1))).not.toBe(normalBorder);
  expect(await color(title)).toBe(await border(groups.nth(1)));
  await page.locator('#grouped-checkout').uncheck();
  expect(await border(groups.first())).toBe(await border(groups.nth(1)));
  await page.locator('#grouped-terminal').check();
  expect(await border(groups.nth(1))).toBe(normalBorder);
  expect(await color(title)).not.toBe(normalTitle);
  await section.locator('[data-card-discard]').click();
  expect(await border(groups.first())).toBe(normalBorder);
  expect(await color(title)).toBe(normalTitle);
});

test('unsaved chip sits directly beside the section title at desktop and mobile widths', async ({ page }) => {
  for (const width of [1100, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/form-grouped.html');
    await page.locator('#grouped-terminal').uncheck();
    const section = page.locator('#card-grouped-payments');
    const title = await section.locator('h3').boundingBox();
    const chip = await section.locator('[data-unsaved]').boundingBox();
    expect(chip.x - (title.x + title.width)).toBeCloseTo(8, 0);
    expect(Math.abs(chip.y + chip.height / 2 - title.y - title.height / 2)).toBeLessThan(2);
  }
});

test('many-edits preview centers the footer summary and discards all seven changes', async ({ page }) => {
  for (const width of [1100, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/form-grouped.html?edits=many');
    const summary = page.locator('[data-save-bar-message]');
    // What shows: the server's own message waits, hidden, for a refusal.
    await expect(summary).toHaveText('7 unsaved changes in Store details, Payments and Receipts', { useInnerText: true });
    await expect(page.locator('.setting-field.is-changed')).toHaveCount(7);
    await expect(page.locator('.settings-card.is-dirty')).toHaveCount(3);
    const placement = await summary.evaluate(el => {
      const range = document.createRange();
      range.selectNodeContents(el);
      const text = range.getBoundingClientRect();
      const box = el.getBoundingClientRect();
      return {
        x: Math.abs(text.x + text.width / 2 - box.x - box.width / 2),
        y: Math.abs(text.y + text.height / 2 - box.y - box.height / 2),
        fits: el.scrollHeight <= el.clientHeight,
      };
    });
    expect(placement.x).toBeLessThan(2);
    expect(placement.y).toBeLessThan(3);
    expect(placement.fits).toBe(true);
    await expect(page.getByRole('button', { name: 'Save settings' })).toBeInViewport();
    await page.getByRole('link', { name: 'Discard changes', exact: true }).click();
    await expect(page.locator('.setting-field.is-changed')).toHaveCount(0);
    await expect(page.locator('#grouped-name')).toHaveValue('Corner store');
  }
});

test('chips use title casing, pill borders and attention colors; Current is plain text', async ({ page }) => {
  await page.goto('/');
  const tag = page.locator('#feedback .tag.state-pending');
  await expect(tag).toHaveCSS('text-transform', 'capitalize');
  await expect(tag).toHaveCSS('border-radius', '999px');
  const payment = page.locator('#compositions .state-pending');
  await expect(payment).toHaveCSS('background-color', await tag.evaluate(el => getComputedStyle(el).backgroundColor));
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

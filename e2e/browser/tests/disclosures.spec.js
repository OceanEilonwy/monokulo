// @ts-check
// Disclosures (issue #8, site.css "Disclosures"): every <details> opens
// with a pointer and shows a focus ring from the keyboard; a plain one reads
// as a text link with a caret that turns over when open, and takes the
// accent colour on hover. A component with a look of its own (the account
// menu here) keeps it, with a pointer, a hover and a focus ring of its own.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}

/** The summary's computed style, and its ::after's content. */
const looks = (summary) => summary.evaluate((el) => {
  const style = getComputedStyle(el);
  return {
    cursor: style.cursor,
    color: style.color,
    decoration: style.textDecorationLine,
    outline: style.outlineStyle,
    after: getComputedStyle(el, '::after').content,
    marker: style.listStyleType,
  };
});

test('a disclosure reads as a link with a caret, with a pointer, a hover and a focus ring', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/account/wallets/import');
  const details = page.locator('details', { hasText: 'Where do I find these keys?' });
  const summary = details.locator('summary');
  await page.mouse.move(0, 0);
  const closed = await looks(summary);
  expect(closed.cursor).toBe('pointer');
  expect(closed.decoration).toBe('underline');
  expect(closed.after).toBe('" ▾"');
  expect(closed.marker).toBe('none');
  await captureCoverageStage(page, 'site-disclosure-closed', test.info(), { group: 'site', shapes: ['desktop'] });

  // Hover: the accent colour.
  await summary.hover();
  expect((await looks(summary)).color).not.toBe(closed.color);

  // Open: the caret turns over.
  await summary.click();
  await expect(details).toHaveAttribute('open', '');
  expect((await looks(summary)).after).toBe('" ▴"');
  await captureCoverageStage(page, 'site-disclosure-open', test.info(), { group: 'site', shapes: ['desktop'] });

  // From the keyboard: a focus ring.
  await page.mouse.move(0, 0);
  await summary.focus();
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Tab');
  await expect(summary).toBeFocused();
  expect((await looks(summary)).outline).toBe('solid');
  await page.keyboard.press('Enter');
  await expect(details).not.toHaveAttribute('open', '');

  // "More options", boxed the same way, reads the same.
  await page.goto(fixture.base_url + '/account/wallets/add');
  const more = page.locator('details', { hasText: 'More options' }).locator('summary');
  const options = await looks(more);
  expect(options.cursor).toBe('pointer');
  expect(options.after).toBe('" ▾"');
});

test('the account menu keeps its own look, with a pointer, a hover and a focus ring', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/');
  const button = page.locator('.acct > summary');
  await page.mouse.move(0, 0);
  const rest = await looks(button);
  expect(rest.cursor).toBe('pointer');
  expect(rest.decoration).toBe('none');
  expect(rest.after).toBe('none');
  const background = () => button.evaluate((el) => getComputedStyle(el).backgroundColor);
  const before = await background();
  await button.hover();
  expect(await background()).not.toBe(before);
  await page.mouse.move(0, 0);
  await button.focus();
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Tab');
  await expect(button).toBeFocused();
  expect((await looks(button)).outline).toBe('solid');
});

/** Where a boxed disclosure sits, in CSS pixels: its summary's text from the
 * box's inner edges, the opened body from the summary's rule and the box,
 * and the box from the elements before and after it. */
const spacing = (details) => details.evaluate((box) => {
  const px = (el, prop) => parseFloat(getComputedStyle(el).getPropertyValue(prop));
  const summary = box.querySelector(':scope > summary');
  const b = box.getBoundingClientRect();
  const s = summary.getBoundingClientRect();
  const inner = {
    top: b.top + box.clientTop, left: b.left + box.clientLeft,
    right: b.left + box.clientLeft + box.clientWidth, bottom: b.bottom - box.clientTop,
  };
  const text = {
    top: s.top + px(summary, 'padding-top'), left: s.left + px(summary, 'padding-left'),
    right: s.right - px(summary, 'padding-right'),
    bottom: s.bottom - px(summary, 'border-bottom-width') - px(summary, 'padding-bottom'),
  };
  const out = {
    summaryTop: text.top - inner.top,
    summaryLeft: text.left - inner.left,
    summaryRight: inner.right - text.right,
    above: b.top - box.previousElementSibling.getBoundingClientRect().bottom,
    below: box.nextElementSibling.getBoundingClientRect().top - b.bottom,
  };
  if (box.open) {
    const body = summary.nextElementSibling;
    const c = body.getBoundingClientRect();
    out.bodyTop = c.top + px(body, 'padding-top') - s.bottom;
    out.bodyLeft = c.left + px(body, 'padding-left') - inner.left;
    out.bodyBottom = inner.bottom - (c.bottom - px(body, 'padding-bottom'));
  } else {
    out.summaryBottom = inner.bottom - text.bottom;
  }
  return out;
});

/** The site's spacing steps (theme.css), in pixels. */
const steps = (page) => page.evaluate(() => {
  const root = getComputedStyle(document.documentElement);
  return Object.fromEntries(['md', 'lg', 'xl'].map((k) => [k, parseFloat(root.getPropertyValue(`--space-${k}`))]));
});

/** Within a pixel of a spacing step. */
const near = (actual, step) => expect(Math.abs(actual - step)).toBeLessThanOrEqual(1);

for (const width of [1280, 390]) {
  test(`"More options" sits a step inside its box and a wide step from its neighbours, at ${width}px`, async ({ page, context }) => {
    await login(context);
    await page.setViewportSize({ width, height: 844 });
    // The same screen in setup's Wallet step and in Add a wallet.
    for (const url of ['/setup/wallet?store_name=New&store_site=new.example', '/account/wallets/add']) {
      await page.goto(fixture.base_url + url);
      const space = await steps(page);
      const details = page.locator('details.more-options');
      await details.scrollIntoViewIfNeeded();
      await page.mouse.move(0, 0);

      // Closed: the summary's text md from the box's top and bottom and lg
      // from its sides; the box xl from the name above and the ways to add
      // a wallet below.
      const closed = await spacing(details);
      near(closed.summaryTop, space.md);
      near(closed.summaryBottom, space.md);
      near(closed.summaryLeft, space.lg);
      near(closed.summaryRight, space.lg);
      near(closed.above, space.xl);
      near(closed.below, space.xl);
      if (url.startsWith('/account')) await captureCoverageStage(page, `site-more-options-closed-${width}`, test.info(), { group: 'site', asIs: true });

      // Open: the summary keeps its inset; the Network field sits md under
      // the summary's rule and above the box's bottom, lg from its side in
      // line with the summary's text; the neighbours stay xl away.
      await details.locator('summary').click();
      await expect(details).toHaveAttribute('open', '');
      await page.mouse.move(0, 0);
      const open = await spacing(details);
      near(open.summaryTop, space.md);
      near(open.summaryLeft, space.lg);
      near(open.summaryRight, space.lg);
      near(open.bodyTop, space.md);
      near(open.bodyBottom, space.md);
      near(open.bodyLeft, space.lg);
      near(open.above, space.xl);
      near(open.below, space.xl);
      if (url.startsWith('/account')) await captureCoverageStage(page, `site-more-options-open-${width}`, test.info(), { group: 'site', asIs: true });
    }

    // "Where do I find these keys?", the other boxed disclosure, has the
    // same inset.
    await page.goto(fixture.base_url + '/account/wallets/import');
    const space = await steps(page);
    const help = await spacing(page.locator('details.keys-help'));
    near(help.summaryTop, space.md);
    near(help.summaryBottom, space.md);
    near(help.summaryLeft, space.lg);
    near(help.summaryRight, space.lg);
  });
}

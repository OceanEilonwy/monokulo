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

  // "More options", with no class of its own, is the same.
  await page.goto(fixture.base_url + '/account/wallets/setup');
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

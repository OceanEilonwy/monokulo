// A store's custom checkout page (crates/monokulo/src/http/checkout_pages.rs):
// writing it, what's wrong with it, and its preview in a sandboxed frame, all
// working without JavaScript. The fixture's DNS always fails, so it marks the
// store's domain verified itself; approving by DNS is covered in-process.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');

let fixture;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

const page_ = '/stores/coverage-store/settings/checkout-page';
const good = '{{status}}\n<h1>Thanks for choosing {{store}}</h1>\n{{qr}}\n<p class="pay">{{amount}} {{fiat}}</p>\n{{address}}';

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}

test('a custom page is refused, fixed and previewed in every state, in a sandbox', async ({ page, context, request }) => {
  await login(context);
  // Without a verified domain, there's nothing to write yet.
  await page.goto(fixture.base_url + page_);
  await expect(page).toHaveURL(/tab=domains/);
  expect((await request.post(`${fixture.base_url}/__coverage/domains/verified`)).status()).toBe(204);

  await page.goto(fixture.base_url + page_);
  // Starts from a stock layout, which is a template too.
  await expect(page.locator('#template')).toHaveValue(/\{\{merchant\}\}/);

  // Everything wrong at once, keeping what was typed.
  await page.locator('#template').fill('{{status}}\n<p>{{countdown}}</p>');
  await page.locator('#css').fill('.pay { background: url(https://cdn.example/a.png) }');
  await page.getByRole('button', { name: 'Check and preview' }).click();
  const problems = page.locator('.template-problems');
  await expect(problems).toContainText("{{countdown}} can't go inside <p>");
  await expect(page.locator('#template')).toHaveValue(/countdown/);

  await page.locator('#template').fill(good);
  await page.locator('#css').fill('.pay { font-weight: 800 }');
  await page.getByRole('button', { name: 'Check and preview' }).click();
  await expect(page).toHaveURL(/checkout-page\/preview/);

  const frames = page.locator('.template-preview iframe');
  await expect(frames).toHaveCount(9);
  await expect(frames.first()).toHaveAttribute('sandbox', '');
  // The page itself, in its frame, with the merchant's words and nothing running.
  const first = page.frameLocator('.template-preview iframe').first();
  await expect(first.locator('h1', { hasText: 'Thanks for choosing' })).toBeVisible();
  await expect(first.locator('[data-part="qr"]')).toBeVisible();
  await expect(first.locator('script')).toHaveCount(0);

  // A link to show someone says what it is, and the frame won't open by itself.
  const link = await page.locator('.mk-copy-value').first().textContent();
  await page.goto(fixture.base_url + link.trim().replace(/^https?:\/\/[^/]+/, '') + '?state=paid');
  await expect(page.getByText('Preview only. This is not a real payment')).toBeVisible();
  const frameUrl = await page.locator('.template-preview iframe').getAttribute('src');
  await expect(page.frameLocator('.template-preview iframe').locator('[data-part="status"]')).toBeVisible();
  const alone = await page.goto(fixture.base_url + frameUrl);
  expect(alone.status()).toBe(403);
});

test('a custom page can be written without JavaScript', async ({ browser, request }) => {
  expect((await request.post(`${fixture.base_url}/__coverage/domains/verified`)).status()).toBe(204);
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(`${fixture.base_url}${page_}?start=tiny`);
  await expect(page.locator('#template')).toHaveValue(/qr small/);
  await page.getByRole('button', { name: 'Check and preview' }).click();
  await expect(page).toHaveURL(/checkout-page\/preview/);
  await expect(page.frameLocator('.template-preview iframe').first().locator('[data-part="status"]')).toBeVisible();
  await context.close();
});

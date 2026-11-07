// A store's custom checkout page (crates/monokulo/src/http/checkout_pages.rs):
// writing it, what's wrong with it, and its preview in a sandboxed frame, all
// working without JavaScript. The fixture's DNS always fails, so it marks the
// store's domain verified itself; approving by DNS is covered in-process.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.describe.configure({ mode: 'default' });
// The shop's page is made up at a public address; its frame is this fixture, on
// loopback, which Chromium otherwise stops a public page reaching.
test.use({ launchOptions: { args: ['--disable-features=LocalNetworkAccessChecks,BlockInsecurePrivateNetworkRequests'] } });
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
  await expect(page.locator('#template')).toHaveValue(/\{\{amount bare\}\}/);

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

// The card sits among the settings form's cards, with its buttons sending forms of
// their own: pressing one sends that form and none of the settings. Before any
// test below publishes a page, as these run in order on one fixture.
test('a draft is discarded from its card on the Checkout tab', async ({ page, context, request }) => {
  await login(context);
  expect((await request.post(`${fixture.base_url}/__coverage/domains/verified`)).status()).toBe(204);
  await page.goto(fixture.base_url + page_);
  await page.locator('#template').fill(good);
  await page.getByRole('button', { name: 'Check and preview' }).click();
  await expect(page).toHaveURL(/checkout-page\/preview/);

  await page.goto(`${fixture.base_url}/stores/coverage-store/settings?tab=checkout`);
  const card = page.locator('#card-custom-page');
  await expect(card).toContainText('Waiting for approval');
  // With a verified domain, Custom among the layouts says how it stands and goes there.
  const custom = page.getByRole('link', { name: /^Custom/ });
  await expect(custom).toContainText('Waiting for approval');
  await custom.click();
  await expect(page).toHaveURL(/#card-custom-page$/);
  await expect(card).toBeInViewport();
  await card.getByRole('button', { name: 'Discard draft' }).click();
  await expect(page.locator('#card-custom-page')).toContainText('Not set up');
});

// The real thing: a page on a merchant's site, framing their checkout. This fixture
// serves plain http under an https page, where a browser would otherwise drop the
// Referer it sends in production, so each frame asks to keep it.
const site = 'https://coverage-shop.example';
const template = `<h1>Fern and Wire</h1>{{status}}{{if awaiting}}<div class="mk-row">{{qr}}<div class="mk-stack mk-gap-sm">{{amount}}{{fiat}}{{address}}</div></div>{{end}}{{if !awaiting}}<p class="done">All done</p>{{end}}`;

async function checkout(page, request, writtenCss = '') {
  expect((await request.post(`${fixture.base_url}/__coverage/domains/verified`)).status()).toBe(204);
  expect((await request.post(`${fixture.base_url}/__coverage/custom-page`, { data: { template, written_css: writtenCss } })).status()).toBe(204);
  const made = await request.post(`${fixture.base_url}/__coverage/orders`);
  const { order_id } = await made.json();
  await page.route(`${site}/**`, (route) => route.fulfill({
    contentType: 'text/html',
    body: `<!doctype html><iframe id="pay" referrerpolicy="unsafe-url" src="${fixture.base_url}/api/v1/checkout/${order_id}" style="width:420px;height:700px"></iframe>`,
  }));
  await page.goto(`${site}/cart`);
  return { order_id, frame: page.frameLocator('#pay') };
}

test('a customer sees the page on the shop, it follows the order live, and a direct visit gets the layout', async ({ page, request }) => {
  const { order_id, frame } = await checkout(page, request);
  await expect(frame.locator('h1', { hasText: 'Fern and Wire' })).toBeVisible();
  await expect(frame.locator('#checkout-root.checkout-custom')).toBeVisible();
  // Regions flip by themselves on the real stream.
  await request.post(`${fixture.base_url}/__coverage/orders/${order_id}/payment?fraction=1`);
  await expect(frame.locator('.done')).toHaveText('All done');
  await expect(frame.locator('[data-part="qr"]')).toHaveCount(0);

  await page.goto(`${fixture.base_url}/api/v1/checkout/${order_id}`);
  await expect(page.locator('#checkout-root')).toBeVisible();
  await expect(page.locator('h1', { hasText: 'Fern and Wire' })).toHaveCount(0);
});

test('rules can\'t turn off what shows how to pay, by accident, and an old version reloads', async ({ page, request }) => {
  const hostile = '#checkout-root [data-part="qr"],#checkout-root [data-part="address"],#checkout-root [data-part="amount"],#checkout-root [data-part="status"]{visibility:hidden!important;opacity:0!important;}';
  const { order_id, frame } = await checkout(page, request, hostile);
  for (const part of ['qr', 'address', 'amount', 'status']) {
    await expect(frame.locator(`[data-part="${part}"]`).first()).toBeVisible();
  }
  // Replaced while it's open: its stream notices the next time the order moves, and
  // the frame moves itself on, staying on the page.
  await request.post(`${fixture.base_url}/__coverage/custom-page`, { data: { template: template.replace('Fern and Wire', 'New version'), written_css: '' } });
  await request.post(`${fixture.base_url}/__coverage/orders/${order_id}/payment?fraction=0.25`);
  await expect(frame.locator('h1', { hasText: 'New version' })).toBeVisible();
});

test('another site can\'t frame it, and loading from elsewhere is refused by the browser', async ({ page, request }) => {
  // What gets past the page's policy to the network, which a route would see.
  const reached = [];
  await page.route('https://evil.example/**', (route) => { reached.push(route.request().url()); route.abort(); });
  // Past the checks on saving, as nothing should get: the page's policy still stops it.
  const { order_id, frame } = await checkout(page, request, '@import url(https://evil.example/a.css);#checkout-root h1{background:url(https://evil.example/b.png)}');
  await expect(frame.locator('h1', { hasText: 'Fern and Wire' })).toBeVisible();
  await page.waitForTimeout(250);
  expect(reached).toEqual([]);

  const other = 'https://other-site.example';
  await page.route(`${other}/**`, (route) => route.fulfill({
    contentType: 'text/html',
    body: `<!doctype html><iframe id="pay" referrerpolicy="unsafe-url" src="${fixture.base_url}/api/v1/checkout/${order_id}"></iframe>`,
  }));
  await page.goto(`${other}/cart`);
  // A site it isn't approved for still takes the payment, on the store's layout.
  await expect(page.frameLocator('#pay').locator('[data-part="qr"]').first()).toBeVisible();
  await expect(page.frameLocator('#pay').locator('h1', { hasText: 'Fern and Wire' })).toHaveCount(0);
});

test('the way through, for the gallery @gallery', async ({ page, context, request }) => {
  await login(context);
  expect((await request.post(`${fixture.base_url}/__coverage/domains/verified`)).status()).toBe(204);
  const stage = (name) => captureCoverageStage(page, `custom-page-${name}`, test.info(), { group: 'custom-page', shapes: ['mobile-portrait', 'desktop'] });
  await page.goto(fixture.base_url + page_);
  await expect(page.locator('#template')).toBeVisible();
  await stage('write');
  await page.locator('#template').fill('{{status}}\n<p>{{countdown}}</p>');
  await page.getByRole('button', { name: 'Check and preview' }).click();
  await expect(page.locator('.template-problems')).toBeVisible();
  await stage('problems');
  await page.locator('#template').fill(good);
  await page.getByRole('button', { name: 'Check and preview' }).click();
  await expect(page).toHaveURL(/checkout-page\/preview/);
  await expect(page.frameLocator('.template-preview iframe').first().locator('[data-part="qr"]')).toBeVisible();
  await stage('preview');
  await page.goto(`${fixture.base_url}${page_}/approve`);
  await expect(page.locator('.template-proof')).toBeVisible();
  await stage('approve');

  // On the shop, as a customer sees it.
  const { frame } = await checkout(page, request);
  await expect(frame.locator('h1', { hasText: 'Fern and Wire' })).toBeVisible();
  await stage('on-the-shop');
});

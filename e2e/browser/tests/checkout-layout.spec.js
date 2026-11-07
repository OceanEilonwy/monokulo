// Where the Detailed checkout's parts sit, in the frames that have gone wrong.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;

test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

async function order(request, data = {}) {
  const response = await request.post(`${fixture.base_url}/api/v1/orders`, {
    headers: { authorization: `Bearer ${fixture.public_key}` },
    data: { amount: '0.011', currency: 'XMR', ...data },
  });
  expect(response.status()).toBe(201);
  return (await response.json()).order_id;
}

test('the merchant name sits by its icon, and a short wide frame fills its second column from the top', async ({ page, request }) => {
  const orderId = await order(request, {
    merchant_order_id: 'RH-202611071546-59',
    show_merchant_order_id: true,
    description: 'Two tote bags and a rather long list of other handmade things',
  });
  await page.goto(`${fixture.base_url}/api/v1/checkout/${orderId}`);
  const columns = async () => Promise.all([page.locator('.pay-col-primary').boundingBox(), page.locator('.pay-col-secondary').boundingBox()]);
  // The heading's own text, not its box: the box is as wide as the description.
  const nameFromIcon = async () => {
    const icon = await page.locator('.merchant > :first-child').boundingBox();
    const name = await page.locator('.merchant h2').evaluate(heading => {
      const text = document.createRange();
      text.selectNodeContents(heading);
      return text.getBoundingClientRect().left;
    });
    return name - (icon.x + icon.width);
  };

  await page.setViewportSize({ width: 800, height: 660 });
  const [primary, secondary] = await columns();
  expect(secondary.x).toBeGreaterThan(primary.x + primary.width);
  expect(secondary.y).toBe(primary.y);
  expect(await nameFromIcon()).toBeLessThan(24);

  // The frame the client makes by default: one column.
  await page.setViewportSize({ width: 420, height: 880 });
  const [above, below] = await columns();
  expect(below.y).toBeGreaterThanOrEqual(above.y + above.height);
  expect(await nameFromIcon()).toBeLessThan(24);
});

test('the share page shows the checkout as one column however large the screen', async ({ page, request }) => {
  const orderId = await order(request);
  for (const viewport of [{ width: 1280, height: 720 }, { width: 1920, height: 1080 }, { width: 3840, height: 2160 }]) {
    await page.setViewportSize(viewport);
    await page.goto(`${fixture.base_url}/api/v1/checkout/${orderId}/share`);
    const frame = page.frameLocator('#checkout-frame');
    await expect(frame.locator('#checkout-root')).toBeVisible();
    const [primary, secondary] = await Promise.all([frame.locator('.pay-col-primary').boundingBox(), frame.locator('.pay-col-secondary').boundingBox()]);
    expect(secondary.y).toBeGreaterThanOrEqual(primary.y + primary.height);
    // The frame takes the checkout's height, so nothing scrolls inside it.
    await expect.poll(() => page.locator('#checkout-frame').evaluate(element => {
      const doc = element.contentDocument.documentElement;
      return doc.scrollHeight - doc.clientHeight;
    })).toBe(0);
  }
});

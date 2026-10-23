// @ts-check
// The embed library on a shop that is on Tor: a page served from a .onion
// address makes its orders, frames the checkout and follows it on
// Monokulo's onion address, never its web address, even when it loaded the
// library from the web address. Monokulo is stood in for by routes, so the
// test needs no Tor.
const { test, expect } = require('../coverage-test');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');
const WEB = 'https://pay.example.com';
const MONOKULO_ONION = 'http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion';
const SHOP_ONION = 'http://shopshopshopshopshopshopshopshopshopshopshopshopshopshop.onion';
const SHOP_WEB = 'https://shop.example.com';
const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-headers': 'authorization, content-type, monokulo-proof',
  'access-control-expose-headers': 'location',
};

function library() {
  const directory = process.env.COVERAGE_ASSETS_DIR || path.join(root, 'crates/monokulo/static');
  return fs.readFileSync(path.join(directory, 'monokulo-client.js'));
}

/** Stands in for the shop and for Monokulo on both its addresses; every
 * request Monokulo is sent is recorded as `METHOD url`. */
async function monokulo(page, { onion = true } = {}) {
  const calls = [];
  await page.route('**/*', async route => {
    const request = route.request();
    const url = new URL(request.url());
    const origin = url.origin;
    if (origin === SHOP_ONION || origin === SHOP_WEB) {
      return route.fulfill({ contentType: 'text/html', body: `<!doctype html><div id="pay"></div><script src="${WEB}/static/monokulo-client.js"></script>` });
    }
    if (request.method() === 'OPTIONS') return route.fulfill({ status: 204, headers: CORS });
    if (url.pathname === '/static/monokulo-client.js') {
      return route.fulfill({ contentType: 'text/javascript', headers: CORS, body: library() });
    }
    calls.push(`${request.method()} ${request.url()}`);
    if (url.pathname === '/api/v1') {
      const links = { self: { href: '/api/v1' } };
      if (onion) links['mk:onion-api'] = { href: `${MONOKULO_ONION}/api/v1` };
      return route.fulfill({ contentType: 'application/prs.hal-forms+json', headers: CORS, body: JSON.stringify({ _links: links }) });
    }
    if (url.pathname === '/api/v1/orders' && request.method() === 'POST') {
      const links = { self: { href: '/api/v1/orders/order_1' }, 'mk:checkout': { href: '/checkout/order_1' } };
      if (onion) links['mk:onion-checkout'] = { href: `${MONOKULO_ONION}/checkout/order_1` };
      return route.fulfill({
        status: 201, contentType: 'application/prs.hal-forms+json', headers: { ...CORS, location: '/api/v1/orders/order_1' },
        body: JSON.stringify({ order_id: 'order_1', address: '8addr', xmr_amount_piconero: 1, amount: '1', currency: 'XMR', expires_at: 0, _links: links }),
      });
    }
    if (url.pathname.endsWith('/events')) {
      return route.fulfill({ contentType: 'text/event-stream', headers: CORS, body: 'retry: 60000\n\nevent: status\ndata: {"status":"pending"}\n\n' });
    }
    if (url.pathname.startsWith('/checkout/')) {
      return route.fulfill({ contentType: 'text/html', body: '<!doctype html><h1>Checkout</h1>' });
    }
    return route.fulfill({ status: 404, headers: CORS, body: '' });
  });
  return calls;
}

async function openShop(page, shop) {
  await page.goto(`${shop}/`);
  await page.waitForFunction(() => window.Monokulo);
}

test('a shop on Tor makes the order and shows the checkout on Monokulo\'s onion address', async ({ page }) => {
  const calls = await monokulo(page);
  await openShop(page, SHOP_ONION);
  const order = await page.evaluate(async () => {
    const created = await window.Monokulo.createOrder({ publicKey: 'pk_test', amount: '1', currency: 'XMR' });
    window.Monokulo.mount('#pay', created);
    return created;
  });
  expect(order.endpoint).toBe(MONOKULO_ONION);
  expect(order.checkoutUrl).toBe(`${MONOKULO_ONION}/checkout/order_1`);
  expect(order.orderUrl).toBe(`${MONOKULO_ONION}/api/v1/orders/order_1`);
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', `${MONOKULO_ONION}/checkout/order_1`);
  await expect.poll(() => calls.some(call => call === `GET ${MONOKULO_ONION}/checkout/order_1/events`)).toBe(true);
  // The web address was asked only where the onion one is.
  expect(calls.filter(call => call.includes(WEB))).toEqual([`GET ${WEB}/api/v1`]);
  expect(calls).toContain(`POST ${MONOKULO_ONION}/api/v1/orders`);
});

test('a shop on Tor showing an order from an earlier visit frames it on the onion address', async ({ page }) => {
  const calls = await monokulo(page);
  await openShop(page, SHOP_ONION);
  await page.evaluate(() => { window.Monokulo.mount('#pay', 'order_7'); });
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', `${MONOKULO_ONION}/checkout/order_7`);
  await expect.poll(() => calls.some(call => call === `GET ${MONOKULO_ONION}/checkout/order_7/events`)).toBe(true);
  expect(calls.filter(call => call.includes(WEB))).toEqual([`GET ${WEB}/api/v1`]);
});

test('a shop on the web never looks for the onion address', async ({ page }) => {
  const calls = await monokulo(page);
  await openShop(page, SHOP_WEB);
  const order = await page.evaluate(() => window.Monokulo.createOrder({ publicKey: 'pk_test', amount: '1', currency: 'XMR' }));
  expect(order.checkoutUrl).toBe(`${WEB}/checkout/order_1`);
  expect(calls).toEqual([`POST ${WEB}/api/v1/orders`]);
});

test('a shop on Tor whose Monokulo has no onion address uses its web address', async ({ page }) => {
  const calls = await monokulo(page, { onion: false });
  await openShop(page, SHOP_ONION);
  const order = await page.evaluate(() => window.Monokulo.createOrder({ publicKey: 'pk_test', amount: '1', currency: 'XMR' }));
  expect(order.endpoint).toBe(WEB);
  expect(order.checkoutUrl).toBe(`${WEB}/checkout/order_1`);
  expect(calls).toEqual([`GET ${WEB}/api/v1`, `POST ${WEB}/api/v1/orders`]);
});

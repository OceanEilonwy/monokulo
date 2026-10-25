// The API explorer's Try it (xtask/src/pages/assets/api/api.js), on the
// explorer built from the API fixture (`cargo xtask pages api --from
// xtask/src/pages/api_fixture`), served on one port, sending to a stand-in
// Monokulo on another that answers CORS as monokulo's embed_cors_layer
// does. What's checked is what keeps a reader's key and page safe: a key
// only goes to an address that is one, an answer is only ever text, a
// problem links only this site's page for it, and Stop ends a stream.
const { test, expect } = require('../coverage-test');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');
const KEY = 'sk_test_0123456789';
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };

let dir, site, stub;
// What reached each server: the site's own (where a relative address
// would land) and the stand-in Monokulo.
const seen = { site: [], stub: [] };
let streamClosed = false;

const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));

test.describe.configure({ mode: 'serial' });
// No smooth scrolling to wait out before a click.
test.use({ reducedMotion: 'reduce' });

test.beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'api-explorer-'));
  execFileSync('cargo', ['xtask', 'pages', 'api', '--site', dir, '--from', 'xtask/src/pages/api_fixture'], { cwd: root, stdio: 'inherit' });
  site = http.createServer((req, res) => {
    seen.site.push({ url: req.url, authorization: req.headers.authorization });
    let file = path.join(dir, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!file.startsWith(dir)) { res.writeHead(403); return res.end(); }
    if (fs.existsSync(file) && fs.statSync(file).isDirectory()) file = path.join(file, 'index.html');
    if (!fs.existsSync(file)) { res.writeHead(404); return res.end(); }
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream' });
    fs.createReadStream(file).pipe(res);
  });
  stub = http.createServer((req, res) => {
    const cors = { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Methods': 'GET,POST,DELETE', 'Access-Control-Allow-Headers': 'content-type,authorization,accept,monokulo-proof' };
    if (req.method === 'OPTIONS') { res.writeHead(204, cors); return res.end(); }
    seen.stub.push({ url: req.url, authorization: req.headers.authorization, cookie: req.headers.cookie });
    const send = (status, type, body) => { res.writeHead(status, { ...cors, 'Content-Type': type }); res.end(body); };
    // Every answer names the request, so a test knows the one it reads.
    const problem = type => send(404, 'application/problem+json', JSON.stringify({ type, title: 'Not found', status: 404, detail: req.url }));
    switch (req.url) {
      case '/api/v1': return send(200, 'application/prs.hal-forms+json', JSON.stringify({ _templates: { 'create-order': {} } }));
      case '/api/v1/orders/hostile': return send(200, 'application/json', JSON.stringify({ note: '<img src=x onerror="window.pwned=1"><script>window.pwned=1</script>', detail: req.url }));
      case '/api/v1/orders/text': return send(200, 'text/html', `<img src=x onerror="window.pwned=1"><script>window.pwned=1</script>${req.url}`);
      case '/api/v1/orders/missing': return problem('https://docs.monokulo.com/problems/not-found');
      case '/api/v1/orders/elsewhere': return problem('https://evil.example/problems/not-found');
      case '/api/v1/orders/script': return problem('javascript:alert(1)');
      case '/api/v1/orders/live/events': {
        res.writeHead(200, { ...cors, 'Content-Type': 'text/event-stream' });
        let tick = 0;
        const timer = setInterval(() => res.write(`event: order\ndata: {"tick":${tick++}}\n\n`), 100);
        req.on('close', () => { clearInterval(timer); streamClosed = true; });
        return;
      }
      default: return problem('https://docs.monokulo.com/problems/not-found');
    }
  });
  site.base = await listen(site);
  stub.base = await listen(stub);
});

test.afterAll(async () => {
  await new Promise(resolve => site?.close(resolve));
  stub?.closeAllConnections();
  await new Promise(resolve => stub?.close(resolve));
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
});

// Opens an operation's Try it with the address and key typed in.
async function openTry(page, op, address) {
  await page.goto(`${site.base}/api/#${op}`);
  await page.locator(`#${op} [data-try]`).click();
  await page.locator('#try-it-address').fill(address);
  await page.locator('#try-it-key').fill(KEY);
}

// Reads the order `id` from the open dialog and waits for its answer.
async function readOrder(page, id) {
  await page.locator('#try-it-path-order_id').fill(id);
  await page.getByRole('button', { name: 'Send request' }).click();
  await expect(page.locator('#try-it-response .api-live > pre')).toContainText(`/api/v1/orders/${id}`);
}

test('an address that is not an absolute http(s) URL is refused, and nothing is sent anywhere', async ({ page }) => {
  await openTry(page, 'order-get', stub.base);
  await page.locator('#try-it-path-order_id').fill('order_1');
  seen.site.length = 0;
  for (const address of ['pay.example.com', '/api', '//evil.example', 'javascript:alert(1)', `${stub.base}/?x=1`, `${stub.base}/#x`, 'https://user:pass@example.com']) {
    await page.locator('#try-it-address').fill(address);
    await page.getByRole('button', { name: 'Check connection' }).click();
    await expect(page.locator('#try-it-state'), address).toHaveText('Not an address');
    await page.getByRole('button', { name: 'Send request' }).click();
    expect(await page.locator('#try-it-address').evaluate(input => input.validationMessage), address).not.toBe('');
  }
  // A schemeless address is told how to start.
  await page.locator('#try-it-address').fill('pay.example.com');
  await page.getByRole('button', { name: 'Check connection' }).click();
  expect(await page.locator('#try-it-address').evaluate(input => input.validationMessage)).toContain('https://');
  await page.waitForTimeout(300);
  await expect(page.locator('#try-it-response .api-live')).toHaveCount(0);
  expect(seen.stub).toEqual([]);
  // Nothing went to this site either (past its own files), the key least
  // of all.
  expect(seen.site.filter(r => r.authorization || !r.url.startsWith('/assets/'))).toEqual([]);
});

test('a hostile answer is shown as text, never as markup', async ({ page }) => {
  await openTry(page, 'order-get', stub.base);
  for (const id of ['hostile', 'text']) {
    await readOrder(page, id);
    await expect(page.locator('#try-it-response .api-live')).toContainText('<img src=x onerror=');
    await expect(page.locator('.api-live img, .api-live script')).toHaveCount(0);
    expect(await page.evaluate(() => window.pwned)).toBeUndefined();
  }
  // It went to the address typed, with the key and no cookie.
  expect(seen.stub.at(-1)).toEqual({ url: '/api/v1/orders/text', authorization: `Bearer ${KEY}`, cookie: undefined });
});

test('a problem links its page only when it is this site\'s', async ({ page }) => {
  await openTry(page, 'order-get', stub.base);
  await readOrder(page, 'missing');
  await expect(page.locator('#try-it-response .api-live a')).toHaveAttribute('href', '../problems/not-found/');
  for (const id of ['elsewhere', 'script']) {
    await readOrder(page, id);
    await expect(page.locator('#try-it-response .api-live a')).toHaveCount(0);
  }
  // The answer stays beside the operation once the dialog closes.
  await page.keyboard.press('Escape');
  await expect(page.locator('#order-get .api-live .mk-http-status')).toHaveText('404');
});

test('Stop ends a stream, keeping what arrived', async ({ page }) => {
  await openTry(page, 'order-events-get', stub.base);
  await page.locator('#try-it-path-order_id').fill('live');
  await page.getByRole('button', { name: 'Send request' }).click();
  const text = page.locator('#try-it-response .api-live > pre');
  await expect(text).toContainText('"tick":1');
  await page.locator('#try-it-response').getByRole('button', { name: 'Stop' }).click();
  await expect.poll(() => streamClosed).toBe(true);
  await expect(page.locator('#try-it-response').getByRole('button', { name: 'Stop' })).toHaveCount(0);
  await expect(text).toContainText('"tick":0');
});

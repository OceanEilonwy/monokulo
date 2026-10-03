// The engine page (docs/engine_visualizer.md) in a real browser, against the
// coverage fixture's engine: following it live, scrubbing the timeline,
// replaying and going live again, zooming, and the page without JavaScript.
// The fixture plays a scripted story into the engine's activity record
// (`POST /__coverage/engine/story`): a block with a payment, a pool payment
// settling, a store catching up and joining the frontier, and a reorg.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const shot = { group: 'engine', shapes: ['mobile-portrait', 'desktop'] };

let fixture;
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture && fixture.process); });

async function openAsAdmin(page, context) {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/status/engine?network=mainnet`);
}

test('the engine page follows the engine live, scrubs, replays and zooms', async ({ page, context }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  await page.setViewportSize({ width: 1600, height: 900 });
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(page.locator('#tl-text')).toHaveText(/^Live/);
  await expect(page.locator('.engine-page a.reload')).toBeHidden();
  await expect(page.locator('#engine-summary')).toContainText('3,412,880');
  await expect(page.locator('.pill.frontier')).toHaveText('Frontier, 41 stores');
  await expect(page.locator('.pill.catchup')).toHaveText('Catching up, 3 stores');

  await page.request.post(`${fixture.base_url}/__coverage/engine/story`);
  const events = page.locator('#engine-events');
  await expect(events).toContainText('Block 3,412,881 scanned for 41 stores and committed, 1 payment found in it.', { timeout: 20000 });
  await expect(events).toContainText('Transaction 6e7f8a9b in the pool pays an order', { timeout: 20000 });
  await expect(page.locator('#orders-sum')).toContainText('unconfirmed');
  await expect(page.locator('#d-reorg')).toHaveAttribute('open', '', { timeout: 20000 });
  await expect(page.locator('#d-reorg summary')).toContainText('Reorg from 3,412,881');
  await captureCoverageStage(page, 'engine-reorg', test.info(), shot);
  await expect(events).toContainText('Rewound: block 3,412,881 deleted', { timeout: 20000 });
  await expect(events).toContainText('caught up and joined the frontier');
  await expect(page.locator('.pill.catchup')).toHaveCount(0);
  await expect(page.locator('#d-reorg')).not.toHaveAttribute('open', '');
  await captureCoverageStage(page, 'engine-live', test.info(), shot);

  // A click on an event goes to it: paused, and the page as it was then.
  await events.locator('tr', { hasText: '1 payment found in it' }).click();
  await expect(page.locator('#tl-text')).toHaveText(/^Paused/);
  await expect(page.locator('#engine-summary')).toContainText('3,412,881');
  await expect(page.locator('.pill.catchup')).toHaveCount(1);
  await expect(events.locator('tr.now')).toContainText('1 payment found in it');

  // Left and right jump between key events; End returns to live.
  await page.locator('#tl').focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.locator('#tl-text')).toHaveText(/^Paused/);
  await page.keyboard.press('End');
  await expect(page.locator('#tl-text')).toHaveText(/^Live/);

  // A click on the track pauses there; Play replays; Live goes back.
  const track = await page.locator('#tl').boundingBox();
  await page.mouse.click(track.x + track.width * 0.5, track.y + 8);
  await expect(page.locator('#tl-text')).toHaveText(/^Paused/);
  await page.locator('#tl-play').click();
  await expect(page.locator('#tl-text')).toHaveText(/^(Replaying|Live)/);
  await page.locator('#tl-live').click();
  await expect(page.locator('#tl-text')).toHaveText(/^Live/);

  // Scrolling zooms; dragging moves along the history.
  const axis = page.locator('#tl-axis');
  const before = await axis.textContent();
  await page.mouse.move(track.x + track.width * 0.5, track.y + 8);
  for (let i = 0; i < 6; i++) await page.mouse.wheel(0, -120);
  await expect(axis).not.toHaveText(before);
  await page.mouse.move(track.x + track.width * 0.6, track.y + 8);
  await page.mouse.down();
  await page.mouse.move(track.x + track.width * 0.9, track.y + 8, { steps: 6 });
  await page.mouse.up();
  await expect(axis.locator('span').last()).toHaveText(/ago$/);

  // A filter hides a tier's rows.
  await page.locator('#engine-filters input[data-tier="blocks"]').uncheck();
  await expect(events.locator('.tierchip.t-blocks')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('without JavaScript the engine page is the network as of now', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  const page = await context.newPage();
  await page.goto(`${fixture.base_url}/status/engine?network=mainnet`);
  await expect(page.locator('#engine-timeline')).toBeHidden();
  await expect(page.locator('.engine-page a.reload')).toBeVisible();
  await expect(page.locator('#engine-summary')).toContainText('Node tip');
  await expect(page.locator('#engine-events tr').first()).toBeVisible();
  await captureCoverageStage(page, 'engine-no-js', test.info(), shot);
  await context.close();
});

test('the engine page is for admins only', async ({ page, context }) => {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  const response = await page.goto(`${fixture.base_url}/status/engine`);
  expect(response.status()).toBe(403);
});

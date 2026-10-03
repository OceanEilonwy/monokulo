// The engine page (docs/engine_visualizer.md) in a real browser, against the
// coverage fixture's engine: following it live, scrubbing the timeline,
// replaying and going live again, moving and resizing the timeline's window,
// the legend, and the page without JavaScript.
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

// One of the playback radio group's options.
let mode;

async function openAsAdmin(page, context) {
  mode = (value) => page.locator(`#tl-modes input[value="${value}"]`);
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/status/engine?network=mainnet`);
}

test('the engine page follows the engine live, scrubs, replays and moves its window', async ({ page, context }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  await page.setViewportSize({ width: 1600, height: 900 });
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(mode('live')).toBeChecked();
  await expect(page.locator('.engine-page a.reload')).toBeHidden();
  await expect(page.locator('#engine-summary')).toContainText('3,412,880');
  await expect(page.locator('#pills .pill.frontier')).toHaveText('Frontier, 41 stores');
  await expect(page.locator('#pills .pill.catchup')).toHaveText('Catching up, 3 stores');
  // The next block holds the node's pool: 23 transactions, 32 % of a block.
  const next = page.locator('#cells .cell.next');
  await expect(next.locator('.cnt')).toHaveText('23');
  await expect(next).toHaveAttribute('title', /23 transactions waiting in the node's pool, 96 kB of the 300 kB/);

  // The legend opens from the (?) and closes with Escape.
  await page.locator('#engine-help summary').click();
  await expect(page.locator('.help-body')).toBeVisible();
  await expect(page.locator('.help-body')).toContainText('hash check');
  await page.keyboard.press('Escape');
  await expect(page.locator('.help-body')).toBeHidden();

  await page.request.post(`${fixture.base_url}/__coverage/engine/story`);
  const events = page.locator('#engine-events');
  await expect(events).toContainText('Block 3,412,881 scanned for 41 stores and committed, 1 payment found in it.', { timeout: 20000 });
  await expect(events).toContainText('Transaction 6e7f8a9b in the pool pays an order', { timeout: 20000 });
  await expect(page.locator('#orders-sum')).toContainText('unconfirmed');
  await expect(events).toContainText('Rewound: block 3,412,881 deleted', { timeout: 20000 });
  await expect(events).toContainText('caught up and joined the frontier');
  await expect(page.locator('#pills .pill.catchup')).toHaveCount(0);
  await expect(page.locator('#d-reorg')).not.toHaveAttribute('open', '');
  await captureCoverageStage(page, 'engine-live', test.info(), shot);

  // The reorg, gone to from its event (live, it can begin and end within
  // one poll on a slow machine): its panel opens by itself while it is open.
  await events.locator('tr', { hasText: "The node's chain differs from block 3,412,881" }).click();
  await expect(mode('paused')).toBeChecked();
  await expect(page.locator('#d-reorg')).toHaveAttribute('open', '');
  await expect(page.locator('#d-reorg summary')).toContainText('Reorg from 3,412,881');
  await captureCoverageStage(page, 'engine-reorg', test.info(), shot);

  // A click on an event goes to it: paused, and the page as it was then.
  await events.locator('tr', { hasText: '1 payment found in it' }).click();
  await expect(mode('paused')).toBeChecked();
  await expect(page.locator('#engine-summary')).toContainText('3,412,881');
  await expect(page.locator('#pills .pill.catchup')).toHaveCount(1);
  await expect(events.locator('tr.now')).toContainText('1 payment found in it');

  // Off live, the playback position's tooltip gives its moment.
  const marker = page.locator('#tl-head');
  await expect(marker).toBeVisible();
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: .* ago/);

  // Left and right jump between key events; End returns to live, and the
  // marker goes.
  await page.locator('#tl-win').focus();
  await page.keyboard.press('ArrowRight');
  await expect(mode('paused')).toBeChecked();
  await page.keyboard.press('End');
  await expect(mode('live')).toBeChecked();
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: live/);

  // A press in the window goes to that moment; Play replays; Live goes back.
  const track = await page.locator('#tl').boundingBox();
  await page.mouse.click(track.x + track.width * 0.5, track.y + 16);
  await expect(mode('paused')).toBeChecked();
  await expect(marker).toBeVisible();
  await expect(page.locator('#tl-text')).toHaveText(/^\d+(s|m \d+s|m) behind live$/);
  await mode('replay').check({ force: true });
  await expect(page.locator('#tl-modes input:checked')).toHaveValue(/^(replay|live)$/);
  await mode('live').check({ force: true });
  await expect(mode('live')).toBeChecked();

  // Before the engine's record starts (it started seconds ago, on a
  // 30-minute bar) the bar is hatched, and says so.
  await page.mouse.move(track.x + 40, track.y + 16);
  await expect(page.locator('#tl-tipbox')).toHaveText(/^No data before .*, when the engine started/);

  // Scrolling over the timeline leaves its window alone.
  const windowBox = page.locator('#tl-win');
  await page.mouse.move(track.x + track.width * 0.5, track.y + 16);
  for (let i = 0; i < 6; i++) await page.mouse.wheel(0, -120);
  // The bar is 30 minutes; the window covers the history held, from the
  // right.
  const whole = await windowBox.boundingBox();
  expect(Math.abs(whole.x + whole.width - (track.x + track.width))).toBeLessThan(3);
  await expect(windowBox).toHaveAttribute('aria-valuetext', /to now$/);

  // While live the playback position is there too; dragging it back
  // pauses there, as Pause would.
  const live = await marker.boundingBox();
  await page.mouse.move(live.x + live.width / 2, live.y + live.height / 2);
  await page.mouse.down();
  await page.mouse.move(live.x - 30, live.y + live.height / 2, { steps: 4 });
  await page.mouse.up();
  await expect(mode('paused')).toBeChecked();
  await mode('live').check({ force: true });
  await expect(mode('live')).toBeChecked();

  // Dragging the window's right handle back ends it in the past: playback
  // pauses at its start. Dragging the middle moves it; dragging the marker
  // scrubs within it.
  const handle = await page.locator('#tl-to').boundingBox();
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle.x - track.width * 0.4, handle.y + handle.height / 2, { steps: 6 });
  await page.mouse.up();
  await expect(mode('paused')).toBeChecked();
  await expect(windowBox).toHaveAttribute('aria-valuetext', /ago to .* ago$/);
  const resized = await windowBox.getAttribute('style');
  const box = await windowBox.boundingBox();
  // Clear of the playback marker, at the window's start.
  await page.mouse.move(box.x + 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 2 + 60, box.y + box.height / 2, { steps: 4 });
  await page.mouse.up();
  await expect(windowBox).not.toHaveAttribute('style', resized);
  const at = await marker.getAttribute('aria-valuetext');
  const head = await marker.boundingBox();
  const inside = await windowBox.boundingBox();
  await page.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
  await page.mouse.down();
  await page.mouse.move(inside.x + inside.width - 4, head.y + head.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect(marker).not.toHaveAttribute('aria-valuetext', at);
  await expect(mode('paused')).toBeChecked();

  // A click on a recent round shows it in the round card, paused; the chip
  // goes back to the live round. The lanes add up to the round.
  await mode('live').check({ force: true });
  const firstRound = page.locator('#ribbon a.rbar').first();
  const number = await firstRound.getAttribute('data-round');
  await firstRound.click();
  const card = page.locator('#engine-round');
  await expect(card.locator('#round-resume')).toHaveText('× Paused');
  await expect(card.locator('#h-round')).toHaveText(`Round ${Number(number).toLocaleString('en-GB')}`);
  await expect(card.locator('a.rbar.pinned')).toHaveCount(1);
  const times = await card.locator('.lane-time').allTextContents();
  const total = await card.locator('.ruler-label').textContent();
  expect(times.reduce((sum, t) => sum + Number(t.replace(/[^0-9]/g, '')), 0)).toBe(Number(total.replace(/[^0-9]/g, '')));
  await card.locator('#round-resume').click();
  await expect(card.locator('#round-resume')).toHaveCount(0);


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

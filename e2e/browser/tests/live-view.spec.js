// The engine page in a real browser, against the coverage fixture's engine:
// following it live, scrubbing the timeline, replaying and going live
// again, moving and resizing the timeline's window, the legend, the network
// picker, the live "Machine and links" strip, the playback control
// following the keys, and a long history kept bounded. The page is an app
// (apps/engine): it draws and words everything from the data monokulo
// sends; how it draws is tested there.
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

test('the engine page follows the engine live, scrubs, replays and moves its window', async ({ page, context }) => {
  // The story takes a few seconds, the run captures screenshots, and a
  // long history is played through at the end: more than the suite's
  // default on a loaded runner.
  test.setTimeout(90_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  // The page's stream, kept for the long history at the end.
  await page.addInitScript(() => {
    const Original = window.EventSource;
    window.EventSource = class extends Original {
      constructor(url, options) {
        super(url, options);
        if (String(url).includes('/status/engine/events')) {
          window.engineSource = this;
          this.addEventListener('history', e => { window.engineHistory = JSON.parse(e.data); });
        }
      }
    };
  });
  await page.setViewportSize({ width: 1600, height: 900 });
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/status/engine`);
  const mode = () => page.locator('#tl-mode');
  const shown = page.locator('#tl-modes .mk-select .mk-button');

  // The network picker goes at once: picking a network loads it.
  await expect(page.locator('.engine-network').getByRole('button', { name: 'Go' })).toBeHidden();
  await Promise.all([
    page.waitForURL(/\/status\/engine\?network=mainnet$/),
    page.evaluate(() => {
      const select = document.getElementById('engine-network');
      select.value = 'mainnet';
      select.dispatchEvent(new Event('change', { bubbles: true }));
    }),
  ]);

  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(mode()).toHaveValue('live');
  await expect(shown).toHaveText(/Live/);
  await expect(page.locator('.engine-page a[data-reload]')).toBeHidden();
  await expect(page.locator('#engine-summary')).toContainText('3,412,880');
  await expect(page.locator('#pills .pill.frontier')).toHaveText('Frontier, 41 stores');
  await expect(page.locator('#pills .pill.catchup')).toHaveText('Catching up, 3 stores');
  // The next block holds the node's pool: 23 transactions, 32 % of a block.
  const next = page.locator('#cells .cell.next');
  await expect(next.locator('.cnt')).toHaveText('23');
  await expect(next).toHaveAttribute('title', /23 transactions waiting in the node's pool, 96 kB of the 300 kB/);

  // The "Machine and links" strip, and the Scanning panel, open, with a
  // short line in its summary.
  const strip = page.locator('#engine-machine');
  for (const tile of ['cpu', 'memory', 'transfer', 'round-trip', 'first-byte', 'block-size']) {
    await expect(strip.locator(`[data-tile="${tile}"]`)).toHaveCount(1);
  }
  // The chip says how long ago the engine reported the figures.
  await expect(page.locator('#machine-age')).toHaveText(/^live · \d+ s ago$/);
  await expect(page.locator('#machine-live .live-dot')).toBeVisible();
  await expect(page.locator('#d-scanning')).toHaveAttribute('open', '');
  await expect(page.locator('#scan-sum')).toHaveText(/^(at the tip|[\d,]+ behind)( · .*)?$/);
  await expect(page.locator('#scan-body dt').first()).toHaveText('Progress');
  const age = async () => Number((await page.locator('#machine-age').textContent()).match(/\d+/)[0]);
  const reportedAge = await age();

  // The legend opens from the (?) and closes with Escape.
  await page.locator('#engine-help summary').click();
  const legend = page.locator('#engine-help .engine-legend');
  await expect(legend).toBeVisible();
  await expect(legend).toContainText('hash check');
  await page.keyboard.press('Escape');
  await expect(legend).toBeHidden();
  await expect(page.locator('#engine-help summary')).toBeFocused();

  const timingDetails = page.locator('#engine-round [data-round-breakdown]');
  await timingDetails.locator('summary').click();
  // Just "Timing details": the round it is for is the card's heading.
  await expect(timingDetails.locator('summary')).toHaveText('Timing details');
  // What the engine records next reaches this page as it happens, pushed
  // to it: the same document, never reloaded.
  await page.evaluate(() => { window.engineStoryWatched = true; });
  const before = await page.locator('#engine-events tr').count();
  await page.request.post(`${fixture.base_url}/__coverage/engine/story`);
  // Paused while a block or the node is lit, the light still goes out;
  // End goes live again.
  const lit = page.locator('.engine-page :is(.flash, .probe, .lit, .spark, .enter)');
  await expect.poll(() => lit.count(), { timeout: 20000 }).toBeGreaterThan(0);
  await page.locator('#tl-win').focus();
  await page.keyboard.press(' ');
  await expect(mode()).toHaveValue('paused');
  await expect(shown).toHaveText(/Pause/);
  await expect(lit).toHaveCount(0, { timeout: 3000 });
  await page.keyboard.press('End');
  await expect(mode()).toHaveValue('live');
  await expect(shown).toHaveText(/Live/);
  const events = page.locator('#engine-events');
  await expect.poll(() => page.locator('#engine-events tr').count(), { timeout: 20000 }).toBeGreaterThan(before);
  await expect(events).toContainText('Block 3,412,881 scanned for 41 stores and committed, 1 payment found in it.', { timeout: 20000 });
  expect(await page.evaluate(() => window.engineStoryWatched)).toBe(true);
  await expect(events).toContainText('Transaction 6e7f8a9b in the pool pays an order', { timeout: 20000 });
  await expect(page.locator('#orders-sum')).toContainText('unconfirmed');
  await expect(events).toContainText('Rewound: block 3,412,881 deleted', { timeout: 20000 });
  await expect(events).toContainText('caught up and joined the frontier');
  await expect(page.locator('#pills .pill.catchup')).toHaveCount(0);
  await expect(page.locator('#d-reorg')).not.toHaveAttribute('open', '');
  await expect(timingDetails).toHaveAttribute('open', '');
  await expect(timingDetails.locator('summary')).toHaveText('Timing details');
  await timingDetails.locator('summary').click();
  await captureCoverageStage(page, 'engine-live', test.info(), shot);
  // Each machine event draws the strip again with the engine's newer
  // report: the age goes back down when one comes.
  let last = reportedAge;
  await expect.poll(async () => { const now = await age(); const fresher = now < last; last = now; return fresher; },
    { message: 'a newer report arrived', timeout: 20_000, intervals: [500] }).toBe(true);
  await captureCoverageStage(page, 'engine-strip', test.info(), shot);

  // The reorg, gone to from its event (live, it can begin and end within
  // one message on a slow machine): its panel opens by itself while it is open.
  await events.locator('tr', { hasText: "The node's chain differs from block 3,412,881" }).click();
  await expect(mode()).toHaveValue('paused');
  await expect(page.locator('#d-reorg')).toHaveAttribute('open', '');
  await expect(page.locator('#d-reorg')).toHaveAttribute('data-alert', '');
  await expect(page.locator('#d-reorg summary')).toContainText('Reorg from 3,412,881');
  await captureCoverageStage(page, 'engine-reorg', test.info(), shot);

  // A click on an event goes to it: paused, and the page as it was then.
  await events.locator('tr', { hasText: '1 payment found in it' }).click();
  await expect(mode()).toHaveValue('paused');
  await expect(page.locator('#engine-summary')).toContainText('3,412,881');
  await expect(page.locator('#pills .pill.catchup')).toHaveCount(1);
  await expect(events.locator('tr[data-now]')).toContainText('1 payment found in it');

  // Off live, the playback position's tooltip gives its moment.
  const marker = page.locator('#tl-head');
  await expect(marker).toBeVisible();
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: .* ago/);

  // Space plays and pauses: the control drawn over the select says so,
  // though from paused to playing and back no option turns on or off to
  // give it away. Left and right jump between key events; End returns to
  // live, and the marker goes.
  await page.locator('#tl-win').focus();
  await page.keyboard.press(' ');
  await expect(mode()).toHaveValue('replay');
  await expect(shown).toHaveText(/Play/);
  await page.keyboard.press(' ');
  await expect(mode()).toHaveValue('paused');
  await expect(shown).toHaveText(/Pause/);
  await page.keyboard.press('ArrowRight');
  await expect(mode()).toHaveValue('paused');
  await page.keyboard.press('End');
  await expect(mode()).toHaveValue('live');
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: live/);

  // A press in the window goes to that moment; Play replays; Live goes back.
  const track = await page.locator('#tl').boundingBox();
  await page.mouse.click(track.x + track.width * 0.5, track.y + 16);
  await expect(mode()).toHaveValue('paused');
  await expect(marker).toBeVisible();
  await expect(page.locator('#tl-text')).toHaveText(/^\d+(s|m \d+s|m) behind live$/);
  await mode().selectOption('replay');
  await expect(page.locator('#tl-mode')).toHaveValue(/^(replay|live)$/);
  await mode().selectOption('live');
  await expect(mode()).toHaveValue('live');

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
  await expect(mode()).toHaveValue('paused');
  await mode().selectOption('live');
  await expect(mode()).toHaveValue('live');

  // Dragging the window's right handle back ends it in the past: playback
  // pauses at its start. Dragging the middle moves it; dragging the marker
  // scrubs within it.
  const handle = await page.locator('#tl-to').boundingBox();
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle.x - track.width * 0.4, handle.y + handle.height / 2, { steps: 6 });
  await page.mouse.up();
  await expect(mode()).toHaveValue('paused');
  await expect(windowBox).toHaveAttribute('aria-valuetext', /ago to .* ago$/);
  const resized = await windowBox.getAttribute('style');
  const box = await windowBox.boundingBox();
  // Clear of the playback marker, at the window's start.
  await page.mouse.move(box.x + 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 2 + 60, box.y + box.height / 2, { steps: 4 });
  await page.mouse.up();
  await expect(windowBox).not.toHaveAttribute('style', resized);
  // Start this drag at the newest retained moment: the earlier window
  // drag may already have clamped the head to the oldest retained event.
  await mode().selectOption('live');
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: live/);
  await mode().selectOption('paused');
  await expect(marker).toHaveAttribute('aria-valuetext', /^Playback position: .* ago/);
  // The moment it shows, exactly (its tooltip's "ago" changes by itself).
  const at = await marker.getAttribute('data-at');
  // Dragged back 40px (about 50s on the 30-minute bar), it shows an
  // earlier moment; the window moves with it.
  const head = await marker.boundingBox();
  await page.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
  await page.mouse.down();
  await page.mouse.move(head.x + head.width / 2 - 40, head.y + head.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => Number(await marker.getAttribute('data-at'))).toBeLessThan(Number(at));
  await expect(mode()).toHaveValue('paused');

  // A click on a recent round shows it in the round card, paused; the chip
  // goes back to the live round. The lanes add up to the round.
  await mode().selectOption('live');
  const firstRound = page.locator('#ribbon .rbar').first();
  const number = await firstRound.getAttribute('data-round');
  await firstRound.click();
  const card = page.locator('#engine-round');
  await expect(card.locator('#round-resume')).toHaveText('× Paused');
  await expect(card.locator('#h-round')).toHaveText(`Round ${Number(number).toLocaleString('en-GB')}`);
  await expect(card.locator('.rbar.pinned')).toHaveCount(1);
  const times = await card.locator('.lane-time').allTextContents();
  const total = await card.locator('.ruler-label').textContent();
  // A part under a millisecond ("<1ms") counts as none.
  const ms = (t) => (t.startsWith('<') ? 0 : Number(t.replace(/[^0-9]/g, '')));
  expect(times.reduce((sum, t) => sum + ms(t), 0)).toBe(ms(total));
  await card.locator('#round-resume').click();
  await expect(card.locator('#round-resume')).toHaveCount(0);

  // A full 30-minute history, followed by another two full histories of
  // distinct events: the page keeps the newest it can hold, stays quick,
  // and its memory stays bounded.
  const cdp = await context.newCDPSession(page);
  await cdp.send('HeapProfiler.collectGarbage');
  const heldBefore = await cdp.send('Runtime.getHeapUsage');
  const timings = [];
  for (let cycle = 0; cycle < 3; cycle++) {
    timings.push(await page.evaluate(cycle => {
      const history = window.engineHistory;
      const now = Date.now();
      const marks = Array.from({ length: 50000 }, (_, index) => ({ seq: 100000 + cycle * 50000 + index,
        at_ms: now - 1700000 + index * 30, round: index, tier: 'blocks', key: index % 5000 === 0,
        what: { kind: 'tx_matched', path: 'fast', txid: `frame${cycle}` } }));
      const frame = { ...history.frame, at_ms: now - 2000, marks, effects: [] };
      const started = performance.now();
      window.engineSource.dispatchEvent(new MessageEvent('frame', { data: JSON.stringify(frame) }));
      return performance.now() - started;
    }, cycle));
    // Played on the page's next draw: its newest events are this frame's.
    await expect(page.locator('#engine-events tbody tr').first()).toContainText(`Transaction frame${cycle} in the pool`);
    await mode().selectOption('paused');
    await expect(mode()).toHaveValue('paused');
    await mode().selectOption('live');
  }
  await cdp.send('HeapProfiler.collectGarbage');
  const heldAfter = await cdp.send('Runtime.getHeapUsage');
  expect(heldAfter.usedSize - heldBefore.usedSize).toBeLessThan(45 * 1024 * 1024);
  expect(Math.max(...timings)).toBeLessThan(1500);
  await expect(page.locator('#engine-events tbody tr')).toHaveCount(60);
  // A dropped stream offers a reload, until the history comes again; a
  // page brought back from the back/forward cache needs none.
  await page.evaluate(() => window.engineSource.dispatchEvent(new Event('error')));
  await expect(page.locator('[data-reload]')).toBeVisible();
  await page.evaluate(() => window.engineSource.dispatchEvent(new MessageEvent('history', { data: JSON.stringify(window.engineHistory) })));
  await expect(page.locator('[data-reload]')).toBeHidden();
  await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pagehide'));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await expect(page.locator('[data-reload]')).toBeHidden();

  // Once the sampler has an hour's first samples, CPU has a figure.
  await expect(strip.locator('[data-tile="cpu"] .v')).not.toHaveText('–', { timeout: 30_000 });
  await page.locator('#engine-filters input[data-tier="blocks"]').uncheck();
  await expect(events.locator('.tierchip.t-blocks')).toHaveCount(0);
  expect(errors).toEqual([]);
});

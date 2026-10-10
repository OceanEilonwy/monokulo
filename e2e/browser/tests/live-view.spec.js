// The engine page (docs/engine_visualizer.md) in a real browser, against the
// coverage fixture's engine: following it live, scrubbing the timeline,
// replaying and going live again, moving and resizing the timeline's window,
// the legend, the chain strip's cells at each width, the round's lanes, the
// network picker, the live "Machine and links" strip, and the page without
// JavaScript.
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
  mode = () => page.locator("#tl-mode");
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/status/engine?network=mainnet`);
}

test('the engine page follows the engine live, scrubs, replays and moves its window', async ({ page, context }) => {
  // The story alone takes about ten seconds, and the run captures eight
  // screenshots: more than the suite's default on a loaded runner.
  test.setTimeout(90_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  await page.setViewportSize({ width: 1600, height: 900 });
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(mode()).toHaveValue('live');
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

  const timingDetails = page.locator('#engine-round .round-breakdown');
  await timingDetails.locator('summary').click();
  const snapshotTitle = await timingDetails.locator('summary').textContent();
  expect(snapshotTitle).toContain('(snapshot)');
  await page.request.post(`${fixture.base_url}/__coverage/engine/story`);
  const events = page.locator('#engine-events');
  await expect(events).toContainText('Block 3,412,881 scanned for 41 stores and committed, 1 payment found in it.', { timeout: 20000 });
  await expect(events).toContainText('Transaction 6e7f8a9b in the pool pays an order', { timeout: 20000 });
  await expect(page.locator('#orders-sum')).toContainText('unconfirmed');
  await expect(events).toContainText('Rewound: block 3,412,881 deleted', { timeout: 20000 });
  await expect(events).toContainText('caught up and joined the frontier');
  await expect(page.locator('#pills .pill.catchup')).toHaveCount(0);
  await expect(page.locator('#d-reorg')).not.toHaveAttribute('open', '');
  await expect(timingDetails).toHaveAttribute('open', '');
  await expect(timingDetails.locator('summary')).toHaveText(snapshotTitle);
  await timingDetails.locator('summary').click();
  await captureCoverageStage(page, 'engine-live', test.info(), shot);

  // The reorg, gone to from its event (live, it can begin and end within
  // one poll on a slow machine): its panel opens by itself while it is open.
  await events.locator('tr', { hasText: "The node's chain differs from block 3,412,881" }).click();
  await expect(mode()).toHaveValue('paused');
  await expect(page.locator('#d-reorg')).toHaveAttribute('open', '');
  await expect(page.locator('#d-reorg summary')).toContainText('Reorg from 3,412,881');
  await captureCoverageStage(page, 'engine-reorg', test.info(), shot);

  // A click on an event goes to it: paused, and the page as it was then.
  await events.locator('tr', { hasText: '1 payment found in it' }).click();
  await expect(mode()).toHaveValue('paused');
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
  const firstRound = page.locator('#ribbon a.rbar').first();
  const number = await firstRound.getAttribute('data-round');
  await firstRound.click();
  const card = page.locator('#engine-round');
  await expect(card.locator('#round-resume')).toHaveText('× Paused');
  await expect(card.locator('#h-round')).toHaveText(`Round ${Number(number).toLocaleString('en-GB')}`);
  await expect(card.locator('a.rbar.pinned')).toHaveCount(1);
  const times = await card.locator('.lane-time').allTextContents();
  const total = await card.locator('.ruler-label').textContent();
  // A part under a millisecond ("<1ms") counts as none.
  const ms = (t) => (t.startsWith('<') ? 0 : Number(t.replace(/[^0-9]/g, '')));
  expect(times.reduce((sum, t) => sum + ms(t), 0)).toBe(ms(total));
  await card.locator('#round-resume').click();
  await expect(card.locator('#round-resume')).toHaveCount(0);


  await page.locator('#engine-filters input[data-tier="blocks"]').uncheck();
  await expect(events.locator('.tierchip.t-blocks')).toHaveCount(0);
  expect(errors).toEqual([]);
});

// The chain strip's cells as laid out: every one drawn, its box, and the
// strip's content box (inside its padding).
async function stripCells(page) {
  return page.evaluate(() => {
    const strip = document.getElementById('strip');
    const box = strip.getBoundingClientRect();
    const style = getComputedStyle(strip);
    const left = box.left + parseFloat(style.paddingLeft), right = box.right - parseFloat(style.paddingRight);
    const cells = [...document.querySelectorAll('#cells > .cell')].map((cell) => {
      const r = cell.getBoundingClientRect();
      return { h: Number(cell.dataset.h), next: cell.classList.contains('next'), left: r.left, right: r.right, top: r.top, width: r.width };
    });
    return { left, right, cells };
  });
}

test('the chain strip shows more blocks when wider, never wider blocks', async ({ page, context }) => {
  test.setTimeout(60_000);
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(page.locator('#cells .cell.next')).toHaveCount(1);
  const counts = [];
  for (const width of [1280, 1024, 800, 390]) {
    await page.setViewportSize({ width, height: 900 });
    // The script fits the strip again once it has been resized.
    await expect.poll(async () => {
      const { left, cells } = await stripCells(page);
      return cells.every((c) => c.left >= left - 0.5);
    }).toBe(true);
    const { left, right, cells } = await stripCells(page);
    const top = cells[0].top;
    for (const cell of cells) {
      // Whole cells only, on one line, inside the strip.
      expect(cell.left, `${width}px: block ${cell.h}`).toBeGreaterThanOrEqual(left - 0.5);
      expect(cell.right, `${width}px: block ${cell.h}`).toBeLessThanOrEqual(right + 0.5);
      expect(Math.abs(cell.top - top), `${width}px: block ${cell.h}`).toBeLessThan(8);
      // A fixed size: 22px, the next block 32px.
      expect(Math.round(cell.width), `${width}px: block ${cell.h}`).toBe(cell.next ? 32 : 22);
    }
    // The newest block is the rightmost, at the right edge.
    const newest = cells.reduce((a, b) => (b.h > a.h ? b : a));
    expect(newest.next).toBe(true);
    expect(Math.max(...cells.map((c) => c.right))).toBe(newest.right);
    expect(right - newest.right).toBeLessThan(1);
    // Room for no other whole cell on the left.
    expect(Math.min(...cells.map((c) => c.left)) - left).toBeLessThan(26);
    counts.push(cells.length);
  }
  for (let i = 1; i < counts.length; i++) expect(counts[i], `${counts}`).toBeLessThan(counts[i - 1]);

  // On a phone the strip is the cells alone: the node list a --space-md
  // (12px) gap under them.
  const gap = await page.evaluate(() => {
    const cells = document.getElementById('cells').getBoundingClientRect();
    const node = document.querySelector('#nodes .node').getBoundingClientRect();
    const space = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--space-md'));
    return { gap: node.top - cells.bottom, space };
  });
  expect(Math.abs(gap.gap - gap.space), JSON.stringify(gap)).toBeLessThanOrEqual(1);
});

test('the round lanes use the width, their chips on their own row at the right', async ({ page, context }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  const lanes = async () => page.evaluate(() => {
    const card = document.getElementById('engine-round');
    const padding = parseFloat(getComputedStyle(card).paddingRight);
    const edge = card.getBoundingClientRect().right - padding;
    return [...card.querySelectorAll('.lanes .track')].map((track) => {
      const t = track.getBoundingClientRect();
      const chip = track.nextElementSibling.querySelector('.engine-chip').getBoundingClientRect();
      const bar = track.querySelector('.bar').getBoundingClientRect();
      return { edge, chipRight: chip.right, chipMiddle: chip.top + chip.height / 2, trackMiddle: t.top + t.height / 2, trackWidth: t.width, trackRight: t.right, chipLeft: chip.left, bar: bar.width };
    });
  });
  for (const width of [1280, 800]) {
    await page.setViewportSize({ width, height: 900 });
    for (const lane of await lanes()) {
      expect(Math.abs(lane.chipRight - lane.edge), `${width}px`).toBeLessThan(1.5);
      expect(Math.abs(lane.chipMiddle - lane.trackMiddle), `${width}px: on the lane's row`).toBeLessThan(4);
      expect(lane.chipLeft, `${width}px`).toBeGreaterThan(lane.trackRight);
    }
  }
  // A phone keeps a mini form of the bars beside the label and the chip.
  await page.setViewportSize({ width: 390, height: 844 });
  for (const lane of await lanes()) {
    expect(lane.trackWidth).toBeGreaterThan(120);
    expect(lane.bar).toBeGreaterThan(0);
    expect(Math.abs(lane.chipMiddle - lane.trackMiddle)).toBeLessThan(4);
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
});

test('the network picker goes at once with JavaScript, with Go without', async ({ page, context, browser }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('.network-go')).toBeHidden();
  await expect(page.locator('mk-select .mk-button').first()).toBeVisible();
  // Picking a network submits the form: the page loads it.
  await Promise.all([
    page.waitForURL(/\/status\/engine\?network=mainnet$/),
    page.evaluate(() => {
      const select = document.getElementById('engine-network');
      select.value = 'mainnet';
      select.dispatchEvent(new Event('change', { bubbles: true }));
    }),
  ]);

  const noJs = await browser.newContext({ javaScriptEnabled: false });
  await noJs.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  const plain = await noJs.newPage();
  await plain.goto(`${fixture.base_url}/status/engine`);
  await expect(plain.locator('#engine-network')).toBeVisible();
  await expect(plain.locator('#engine-network option[value="testnet"]')).toBeDisabled();
  await plain.locator('.network-go').click();
  await expect(plain).toHaveURL(/\/status\/engine\?network=mainnet$/);
  await noJs.close();
});

test('the machine strip and the Scanning panel follow the engine live', async ({ page, context }) => {
  test.setTimeout(60_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  await openAsAdmin(page, context);
  const strip = page.locator('#engine-machine');
  for (const tile of ['cpu', 'memory', 'transfer', 'round-trip', 'first-byte', 'block-size']) {
    await expect(strip.locator(`[data-tile="${tile}"]`)).toHaveCount(1);
  }
  // The chip says how long ago the engine reported the figures.
  await expect(page.locator('#machine-age')).toHaveText(/^live · \d+ s ago$/);
  await expect(page.locator('#machine-live .live-dot')).toBeVisible();
  // The Scanning panel is open, with a short line in its summary.
  await expect(page.locator('#d-scanning')).toHaveAttribute('open', '');
  await expect(page.locator('#scan-sum')).toHaveText(/^(at the tip|[\d,]+ behind)( · .*)?$/);
  // Each machine event draws them again with the engine's newer report:
  // the age goes back down when one comes.
  const age = async () => Number((await page.locator('#machine-age').textContent()).match(/\d+/)[0]);
  let last = await age(), fresher = false;
  for (let i = 0; i < 40 && !fresher; i++) {
    await page.waitForTimeout(500);
    const now = await age();
    fresher = now < last;
    last = now;
  }
  expect(fresher, 'a newer report arrived').toBe(true);
  await expect(page.locator('#scan-body dt').first()).toHaveText('Progress');
  // Once the sampler has an hour's first samples, CPU has a figure.
  await expect(strip.locator('[data-tile="cpu"] .v')).not.toHaveText('–', { timeout: 30_000 });
  // The panels are a grid under the round: an open one spans the row.
  const [scanning, reorg] = await Promise.all(['#d-scanning', '#d-reorg'].map((id) => page.locator(id).boundingBox()));
  const panels = await page.locator('#engine-panels').boundingBox();
  expect(Math.abs(scanning.width - panels.width)).toBeLessThan(1);
  expect(reorg.width).toBeLessThan(panels.width / 2);
  await captureCoverageStage(page, 'engine-strip', test.info(), { group: 'engine', shapes: ['mobile-portrait', 'tablet-portrait', 'desktop'] });
  expect(errors).toEqual([]);
});

// A theme spacing step, in pixels.
const space = (page, step) => page.evaluate((step) => parseFloat(getComputedStyle(document.documentElement).getPropertyValue(`--space-${step}`)), step);

test('the chain strip labels its blocks from above, at the right blocks', async ({ page, context }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  await expect(page.locator('#pills .pill.catchup')).toHaveText('Catching up, 3 stores');
  const sm = await space(page, 'sm');
  const layout = () => page.evaluate(() => {
    const box = (el) => el.getBoundingClientRect();
    const cells = [...document.querySelectorAll('#cells > .cell')].map((c) => ({ h: Number(c.dataset.h), ...JSON.parse(JSON.stringify(box(c))) }));
    const top = Math.min(...cells.map((c) => c.top));
    const labels = [...document.querySelectorAll('#pills .pill, #chain-marks .m-tip, #chain-marks .m-hw')].map((el) => box(el).bottom);
    const axis = [...document.querySelectorAll('#chain-axis span')].map((el) => box(el).top);
    const win = document.querySelector('#chain-marks .m-win');
    const catchup = document.querySelector('#pills .pill.catchup');
    // A label's tick (a pseudo-element): its left and right, from the
    // label's box and the tick's offsets inside the label's border.
    const tickOf = (el, pseudo) => {
      if (!el) return null;
      const r = box(el), style = getComputedStyle(el), tick = getComputedStyle(el, pseudo);
      const width = parseFloat(tick.borderLeftWidth) || parseFloat(tick.width);
      const left = tick.left !== 'auto'
        ? r.left + parseFloat(style.borderLeftWidth) + parseFloat(tick.left)
        : r.right - parseFloat(style.borderRightWidth) - parseFloat(tick.right) - width;
      return { left, right: left + width, middle: left + width / 2 };
    };
    const ticks = {
      catchup: tickOf(catchup, '::before'),
      frontier: tickOf(document.querySelector('#pills .pill.frontier'), '::before'),
      tip: tickOf(document.querySelector('#chain-marks .m-tip'), '::after'),
    };
    return { cells, top, bottom: Math.max(...cells.map((c) => c.bottom)), labels, axis, win: win && box(win).bottom, tick: ticks.catchup && ticks.catchup.middle, ticks };
  });
  // Wide: the catching-up stores' block is drawn (beyond the cut), and the
  // tick lands on it.
  await page.setViewportSize({ width: 1280, height: 900 });
  await expect.poll(async () => (await layout()).cells.length).toBeGreaterThan(30);
  // (Pills and marks glide to their blocks.)
  await page.waitForTimeout(600);
  const l = await layout();
  for (const bottom of l.labels) expect(bottom, 'every label above the blocks').toBeLessThanOrEqual(l.top);
  for (const top of l.axis) expect(top, 'the block numbers under them').toBeGreaterThanOrEqual(l.bottom);
  // The reorg window's line a --space-sm step above the blocks.
  expect(Math.abs(l.top - l.win - sm), `${l.top} ${l.win}`).toBeLessThanOrEqual(1);
  const onto = l.cells.filter((c) => Math.abs(c.left + c.width / 2 - l.tick) <= 1.5);
  expect(onto.length, 'the tick lands on a block').toBe(1);
  expect(onto[0].h).toBeLessThan(Math.max(...l.cells.map((c) => c.h)) - 20);
  // The frontier and the node's tip on one block: their ticks are the same
  // two pixels, one line of the normal width, not two side by side.
  expect(Math.abs(l.ticks.frontier.left - l.ticks.tip.left), JSON.stringify(l.ticks)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(l.ticks.frontier.right - l.ticks.tip.right), JSON.stringify(l.ticks)).toBeLessThanOrEqual(0.5);
  // Narrow: no cut, and the stores are behind the oldest block drawn: the
  // tick is at it, the left edge, not further right.
  await page.setViewportSize({ width: 800, height: 900 });
  await expect.poll(async () => (await layout()).cells.length).toBeLessThan(20);
  // (The pill glides there.)
  await expect.poll(async () => {
    const now = await layout();
    const oldest = now.cells.reduce((a, b) => (a.h < b.h ? a : b));
    return oldest.h > onto[0].h && Math.abs(now.tick - (oldest.left + oldest.width / 2)) <= 1.5;
  }).toBe(true);
  // The node list is the network's nodes, and nothing else.
  await expect(page.locator('#nodes > .node')).toHaveCount(2);
  await expect(page.locator('#node-call')).toHaveCount(0);
  await expect(page.locator('#nodes > .node .label')).toHaveText(['node-a.example:18081', 'node-b.example:18081']);
});

test('the round card has room: inset bars, its end marker, chips apart', async ({ page, context }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  const [xs, lg] = [await space(page, 'xs'), await space(page, 'lg')];
  const card = await page.evaluate(() => {
    const box = (el) => JSON.parse(JSON.stringify(el.getBoundingClientRect()));
    const round = document.getElementById('engine-round');
    const lanes = [...round.querySelectorAll('.lanes .track')].map((track) => ({
      track: box(track),
      share: track.querySelector('.share') && box(track.querySelector('.share')),
      bars: [...track.querySelectorAll('.bar')].map((bar) => ({
        ...box(bar),
        cls: bar.className,
        shapes: [...bar.children].map((shape) => ({ cls: shape.className, ...box(shape), display: getComputedStyle(shape).display })),
      })),
      chip: box(track.nextElementSibling.querySelector('.engine-chip')),
    }));
    const label = round.querySelector('.ruler-label');
    const lastBar = round.querySelector('.bar.last');
    const marker = getComputedStyle(lastBar, '::after');
    const lastBox = lastBar.getBoundingClientRect();
    return {
      lanes,
      label: box(label),
      lasts: round.querySelectorAll('.bar.last').length,
      marker: { height: parseFloat(marker.height), width: parseFloat(marker.width), left: lastBox.right - parseFloat(marker.right) - parseFloat(marker.width) },
      lastRight: lastBox.right,
      lastTime: box(lastBar.nextElementSibling),
      ruleLine: getComputedStyle(label, '::before').content,
      lanesBox: box(round.querySelector('.lanes')),
      details: box(round.querySelector('.round-breakdown')),
    };
  });
  for (const [i, lane] of card.lanes.entries()) {
    // The chips apart, each on its lane's row.
    if (i) expect(lane.chip.top, `lane ${i}`).toBeGreaterThanOrEqual(card.lanes[i - 1].chip.bottom);
    if (!lane.share) continue;
    // Inside the dashed outline with a --space-xs gap all round: nothing
    // covers it.
    for (const bar of lane.bars) {
      expect(bar.left - (lane.share.left + 1.5), `lane ${i} start`).toBeGreaterThanOrEqual(xs - 0.5);
      expect(bar.top - (lane.share.top + 1.5), `lane ${i} top`).toBeGreaterThanOrEqual(xs - 0.5);
      expect(lane.share.bottom - 1.5 - bar.bottom, `lane ${i} bottom`).toBeGreaterThanOrEqual(xs - 0.5);
    }
  }
  // Blocks' unit, then Mempool, Settlement and Upkeep (306ms, 3 % of the
  // round's 10s budget), then its cache carry: on a wide track a thread
  // crosses the gap to an outline over the carry's own time, the outline
  // exactly the solid part's height.
  const blocks = card.lanes[1].bars;
  expect(blocks).toHaveLength(1);
  expect(blocks[0].cls).toBe('bar last');
  const [fill, thread, work] = blocks[0].shapes;
  expect([fill.cls, thread.cls, work.cls]).toEqual(['fill', 'thread band', 'work band']);
  expect(work.height).toBe(fill.height);
  expect(thread.height).toBe(1);
  expect(Math.abs(thread.left - fill.right)).toBeLessThanOrEqual(1);
  expect(work.left - fill.right, 'split: the outline over the carry alone').toBeGreaterThan(20);
  // The marker 3px past the end of the segment that finished last, its
  // time and the round's total moved with it.
  expect(Math.abs(card.marker.left - card.lastRight - 3), JSON.stringify(card.marker)).toBeLessThanOrEqual(0.5);
  expect(card.lastTime.left).toBeGreaterThanOrEqual(card.marker.left + card.marker.width + 3);
  const middle = card.marker.left + card.marker.width / 2;
  expect(Math.abs(card.label.left + card.label.width / 2 - middle), `label ${JSON.stringify(card.label)} marker ${middle}`).toBeLessThanOrEqual(1.5);
  // The marker is on that one segment, in its lane only: no line running
  // across the other lanes to the total.
  expect(card.lasts).toBe(1);
  expect(card.marker.width).toBe(2);
  expect(card.marker.height).toBeLessThanOrEqual(card.lanes[1].track.height);
  expect(card.ruleLine).toBe('none');
  expect(card.label.top).toBeGreaterThan(card.lanes[4].track.bottom);
  // Room above Timing details.
  expect(Math.abs(card.details.top - card.lanesBox.bottom - lg), `${card.details.top} ${card.lanesBox.bottom}`).toBeLessThanOrEqual(1);
  // The playback row centred in its card.
  const centred = await page.evaluate(() => {
    const cardBox = document.getElementById('engine-timeline');
    const style = getComputedStyle(cardBox);
    const outer = cardBox.getBoundingClientRect();
    const middle = outer.top + parseFloat(style.borderTopWidth) + parseFloat(style.paddingTop) + (cardBox.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom)) / 2;
    const modes = document.getElementById('tl-modes').getBoundingClientRect();
    return { middle, modes: modes.top + modes.height / 2 };
  });
  expect(Math.abs(centred.middle - centred.modes), JSON.stringify(centred)).toBeLessThanOrEqual(1.5);
});

test('on a phone a gap between the thresholds is joined', async ({ page, context }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  const blocks = await page.evaluate(() => {
    const bar = document.querySelector('#engine-round .track.t-blocks .bar');
    const track = bar.closest('.track').getBoundingClientRect();
    const [fill, thread, work] = [...bar.children].map((el) => ({ ...JSON.parse(JSON.stringify(el.getBoundingClientRect())), display: getComputedStyle(el).display, cls: el.className }));
    return { track: track.width, fill, thread, work };
  });
  expect(blocks.track).toBeLessThan(480);
  expect(blocks.work.cls).toBe('work band');
  // The thread goes and the outline starts where the solid part ends.
  expect(blocks.thread.display).toBe('none');
  expect(Math.abs(blocks.work.left - blocks.fill.right)).toBeLessThanOrEqual(0.5);
  expect(blocks.work.height).toBe(blocks.fill.height);
});

// The script's label rule, run on the cases the server's is tested with
// (views/label_levels.json): one rule, two places, kept in step.
test('the script stacks labels by the same rule as the server', async () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const root = path.resolve(__dirname, '../../../crates/monokulo');
  const source = fs.readFileSync(path.join(root, 'static/engine-view.js'), 'utf8');
  const gap = source.match(/const LABEL_GAP_PX = \d+;/)[0];
  const body = source.match(/function stackLabels\(spans\) \{[\s\S]*?\n {2}\}\n/)[0];
  const stackLabels = new Function(`${gap}\n${body}\nreturn stackLabels;`)();
  const cases = JSON.parse(fs.readFileSync(path.join(root, 'src/views/label_levels.json'), 'utf8'));
  expect(cases.length).toBeGreaterThanOrEqual(7);
  for (const c of cases) expect(stackLabels(c.spans), c.case).toEqual([c.levels, c.used]);
});

test('the chain strip labels take as few levels as they can, with 1px leaders', async ({ page, context }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  const read = () => page.evaluate(() => {
    const strip = document.getElementById('strip');
    const labels = [...document.querySelectorAll('#chain-marks .m-tip, #chain-marks .m-hw, #pills .pill')]
      .filter((el) => el.offsetParent !== null)
      .map((el) => {
        const r = el.getBoundingClientRect();
        const leader = getComputedStyle(el, el.classList.contains('pill') ? '::before' : '::after');
        return { text: el.textContent, level: Number(getComputedStyle(el).getPropertyValue('--level')), left: r.left, right: r.right, top: r.top, bottom: r.bottom, leader: leader.borderLeftWidth };
      });
    return { labels, levels: Number(getComputedStyle(strip).getPropertyValue('--levels')), height: strip.getBoundingClientRect().height };
  });
  const gap = 4;
  // The user's case: stores catching up far to the left, the frontier and
  // the node's tip on one block. Only the frontier goes up.
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.waitForTimeout(700);
  let s = await read();
  const by = (text) => s.labels.find((l) => l.text.startsWith(text));
  expect([by('node tip').level, by('Frontier').level, by('Catching up').level]).toEqual([0, 1, 0]);
  expect(s.levels).toBe(2);
  // The strip is as tall as the levels it uses.
  expect(s.height).toBe(32 + (s.levels - 1) * 22 + 46);
  for (const width of [1280, 800]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(700);
    s = await read();
    expect(s.labels.length).toBeGreaterThan(1);
    for (const [i, a] of s.labels.entries()) {
      expect(a.leader, `${a.text}: a 1px leader`).toBe('1px');
      for (const b of s.labels.slice(i + 1)) {
        const apart = a.right + gap - 0.5 <= b.left || b.right + gap - 0.5 <= a.left || a.bottom <= b.top || b.bottom <= a.top;
        expect(apart, `${width}px: ${a.text} and ${b.text} apart`).toBe(true);
      }
      // Up a level only because it would overlap a label on each one below.
      for (let below = 0; below < a.level; below++) {
        const blocked = s.labels.some((b) => b !== a && b.level === below && !(b.right + gap <= a.left + 0.5 || a.right + gap <= b.left + 0.5));
        expect(blocked, `${width}px: ${a.text} could go down to level ${below}`).toBe(true);
      }
    }
    expect(s.levels).toBe(Math.max(1, ...s.labels.map((l) => l.level + 1)));
  }
  // A phone shows no labels: the strip is the cells alone.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(300);
  s = await read();
  expect(s.labels).toEqual([]);
});

test('the timeline scrub bar reaches the playback control', async ({ page, context }) => {
  await openAsAdmin(page, context);
  await expect(page.locator('#engine-timeline')).toBeVisible();
  const xl = await space(page, 'xl');
  const xs = await space(page, 'xs');
  const measure = () => page.evaluate(() => {
    const box = (el) => el.getBoundingClientRect();
    const card = document.getElementById('engine-timeline');
    const style = getComputedStyle(card);
    return {
      bar: box(document.getElementById('tl')),
      win: box(document.getElementById('tl-win')),
      handle: box(document.getElementById('tl-to')),
      head: box(document.getElementById('tl-head')),
      modes: box(document.getElementById('tl-modes')),
      inner: box(card).right - parseFloat(style.paddingRight) - parseFloat(style.borderRightWidth),
      innerLeft: box(card).left + parseFloat(style.paddingLeft) + parseFloat(style.borderLeftWidth),
    };
  });
  for (const mode of ['live', 'paused']) {
    // Pause is the control's longest option, and paused the readout shows.
    if (mode === 'paused') await page.locator('#tl-mode').selectOption('paused');
    for (const width of [1280, 820]) {
      await page.setViewportSize({ width, height: 900 });
      await page.waitForTimeout(300);
      const m = await measure();
      expect(Math.abs(m.modes.left - m.bar.right - xl), `${mode} ${width}px: ${m.bar.right} to ${m.modes.left}`).toBeLessThanOrEqual(1);
      // The window ends at the bar's new right edge, its right handle
      // (16px outside the window) still clear of the control.
      if (mode === 'live') {
        expect(Math.abs(m.win.right - m.bar.right), `${width}px`).toBeLessThanOrEqual(3);
        expect(Math.abs(m.head.left + m.head.width / 2 - m.bar.right), `${width}px: the head marker at now, 1.5s behind`).toBeLessThanOrEqual(3);
      }
      expect(m.modes.left - m.handle.right, `${mode} ${width}px: the handle clear of the control`).toBeGreaterThanOrEqual(xs);
    }
  }
  // A phone: the bar takes the card's whole width, the control under it.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(300);
  const m = await measure();
  expect(Math.abs(m.bar.right - m.inner)).toBeLessThanOrEqual(1);
  expect(Math.abs(m.bar.left - m.innerLeft)).toBeLessThanOrEqual(1);
  expect(m.modes.top).toBeGreaterThan(m.bar.bottom);
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
  // The server stacks the labels by the same rule: only the frontier,
  // on the node tip's block, goes up a level.
  await expect(page.locator('#strip')).toHaveAttribute('style', '--levels:2');
  await expect(page.locator('#chain-marks .m-tip')).toHaveAttribute('style', /--level:0$/);
  await expect(page.locator('#pills .pill.frontier')).toHaveAttribute('style', /--level:1$/);
  await expect(page.locator('#pills .pill.catchup')).toHaveAttribute('style', /--level:0$/);
  await captureCoverageStage(page, 'engine-no-js', test.info(), shot);
  await context.close();
});

test('the engine page is for admins only', async ({ page, context }) => {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  const response = await page.goto(`${fixture.base_url}/status/engine`);
  expect(response.status()).toBe(403);
});

// @ts-check
// The status page's network card (issues #15-#18): the network's name as
// the card's title with its states beside it, "Watch it live" for admins,
// one Nodes table with each node's proof-of-work verdict, the scanner on
// one line, and proof of work as a sentence over the last 30 blocks, or
// the held box, following the engine live. Captures each state for the
// gallery, desktop and phone, light and dark.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'status';
const SHAPES = ['desktop', 'mobile-portrait'];
let fixture;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context, admin = true) {
  await context.addCookies([{ name: 'session', value: admin ? fixture.admin_session : fixture.session, url: fixture.base_url }]);
}

/** Holds the fixture's mainnet status at `story` (following, lagging, held). */
async function story(name) {
  const response = await fetch(`${fixture.base_url}/__coverage/status/${name}`, { method: 'POST' });
  expect(response.status).toBe(204);
}

const card = (page) => page.locator('#network-mainnet');

/**
 * The settled block's ring (its box grown by the outline and its offset)
 * stays clear of the blocks either side, and every block is one width.
 */
async function expectRingClear(page) {
  const found = await card(page).locator('.pips').evaluate((pips) => {
    const settles = pips.querySelector('.pip[data-settles]');
    const style = getComputedStyle(settles);
    const reach = parseFloat(style.outlineWidth) + parseFloat(style.outlineOffset);
    const box = settles.getBoundingClientRect();
    const ring = { left: box.left - reach, right: box.right + reach, top: box.top - reach, bottom: box.bottom + reach };
    const side = (el) => el && el.getBoundingClientRect();
    const before = side(settles.previousElementSibling);
    const after = side(settles.nextElementSibling);
    const widths = [...pips.querySelectorAll('.pip')].map((pip) => Math.round(pip.getBoundingClientRect().width * 100) / 100);
    return { reach, ring, before: before && { right: before.right }, after: after && { left: after.left }, widths };
  });
  expect(found.reach).toBeGreaterThan(0);
  expect(found.before, 'a block before the settled one').toBeTruthy();
  expect(found.after, 'a block or the tip after the settled one').toBeTruthy();
  expect(found.ring.left).toBeGreaterThan(found.before.right);
  expect(found.ring.right).toBeLessThan(found.after.left);
  // One width, give or take the layout's sub-pixel rounding.
  expect(Math.max(...found.widths) - Math.min(...found.widths), found.widths.join(' ')).toBeLessThan(0.1);
}

test('an admin sees the card titled Mainnet, its nodes and proof of work 3 behind the tip', async ({ page, context }) => {
  await story('following');
  await login(context);
  await page.goto(fixture.base_url + '/status');
  const head = card(page).locator('.net-head');
  await expect(head.getByRole('heading', { level: 2 })).toHaveText('Mainnet');
  await expect(head.locator('.mk-status-text')).toHaveText(['scanning', 'proof checked']);
  // The title is clearly larger than the section heads under it.
  const sizes = await card(page).evaluate((box) => [
    parseFloat(getComputedStyle(box.querySelector('h2')).fontSize),
    parseFloat(getComputedStyle(box.querySelector('h3.caps')).fontSize),
  ]);
  expect(sizes[0]).toBeGreaterThan(sizes[1] * 1.5);
  const watch = head.getByRole('link', { name: 'Watch it live' });
  await expect(watch).toHaveAttribute('href', '/status/engine?network=mainnet');
  await expect(watch).toHaveClass(/mk-btn/);
  await expect(watch).not.toHaveAttribute('data-variant');
  await expect(page.locator('a[href^="/status/engine"]')).toHaveCount(1);

  const table = card(page).locator('table[data-table="nodes"]');
  await expect(table.locator('thead th')).toHaveText(['Node', 'Use', 'Proof of work', 'Height', 'Reachable']);
  const rows = table.locator('tbody tr');
  await expect(rows.locator('td.card-status')).toHaveText(['in use', 'standby', 'left out', 'standby']);
  await expect(rows.locator('.verdict')).toHaveText([
    'on the proven chain', 'ahead, being checked', 'served a block that breaks the rules', 'not answering']);
  await expect(rows.nth(2).locator('.sub')).toHaveText("block 3,412,860's proof of work doesn't meet its difficulty.");

  await expect(card(page).locator('.scanner-line')).toContainText('· 41,203 ticks · 3 stores scanned last tick');
  await expect(card(page).locator('.pline')).toHaveText('Orders settle on blocks up to 3,412,878, proven by the engine itself. 3 behind the tip');
  await expect(card(page).locator('.pips .pip')).toHaveCount(30);
  await expect(card(page).locator('.pips .pip.seen')).toHaveCount(3);
  await expect(card(page).locator('[data-pip-anchor]')).toHaveText('Anchored at block 3,412,160, 2d ago: every block since then is checked.');
  await expectRingClear(page);
  await captureCoverageStage(page, 'status-network-card-following', test.info(), { group: GROUP, shapes: SHAPES });

  const facts = card(page).locator('details[data-proof-facts]');
  await facts.locator('summary').click();
  await expect(facts.locator('dt')).toHaveText(['Anchor', 'Checked', 'RandomX keys', 'Last checked']);
  await expect(facts.locator('dd').first()).toHaveText('Block 3,412,160, 2 of 3 nodes agreed, 2d ago');
  await captureCoverageStage(page, 'status-network-card-how-its-checked', test.info(), { group: GROUP, shapes: SHAPES });

  // The card follows the engine live, the new markup swapped in whole: the
  // stream looks again as often as the cached status can change (a second
  // in the fixture, engine.status_cache_ms).
  await story('lagging');
  const chip = card(page).locator('.pline .tag');
  await expect(chip).toHaveText('14 behind the tip');
  await expect(chip).toHaveClass('tag tag-unknown');
  await expect(card(page).locator('.pips .pip.seen')).toHaveCount(14);
  await expect(card(page).locator('.net-head h2')).toHaveText('Mainnet');
  await expect(page.locator('#status-live')).toHaveCount(1);
  await captureCoverageStage(page, 'status-network-card-lagging', test.info(), { group: GROUP, shapes: SHAPES });

  // Held: one box gives the reason and a button to take a new anchor.
  await story('held');
  const held = card(page).locator('.proof-held');
  await expect(held).toContainText('Every node\'s chain left the proven one more than 720 blocks back');
  await expect(card(page).locator('.net-head .mk-status-text')).toHaveText(['scanning', 'settlement held']);
  await expect(held.getByRole('button', { name: 'Take a new anchor' })).toBeVisible();
  await expect(held.locator('form')).toHaveAttribute('action', '/admin/proof/mainnet/reanchor');
  await expect(card(page).locator('.pips')).toHaveCount(0);
  await captureCoverageStage(page, 'status-network-card-held', test.info(), { group: GROUP, shapes: SHAPES });
});

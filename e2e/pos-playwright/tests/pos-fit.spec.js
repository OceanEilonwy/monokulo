// @ts-check
// Regression guard for the POS terminal screen's own "never needs to
// scroll" requirement: on no phone/tablet size, in either orientation, on
// any screen state (the keypad, a filled amount + note, the charge/QR
// screen, or the paid/error confirmation overlay) should the page ever
// need scrolling. This was violated for real, twice over, while building
// this screen - see `templates/pos.html.hbs`'s own `updateKeypadSize`/
// `@media (max-height: 480px)` comments for the two distinct bugs found
// and fixed (a circular size measurement, and a CSS cascade-order bug that
// silently discarded the fix for the charge screen).
//
// Deliberately generic (`scrollHeight <= innerHeight`, not "the Charge
// button is at pixel Y"), so this keeps working as a regression guard even
// after the layout itself changes - it asserts the *requirement*, not
// today's specific implementation of it.
//
// Uses the same real backend `global-setup.js` boots for `pos.spec.js`
// (one real signed-up account, one real connected store) - this suite
// needs no real payment at all, just the page rendering, so it's fast and
// runs every time regardless of stagenet's own state.
//
// Runs on Chromium always; also on WebKit (real Safari engine - the
// engine iPhone/iPad actually use, and different enough from Chromium on
// flex/grid sizing that it's worth checking directly) when installed.
// `npx playwright install webkit` (plus, on Linux, its own system
// libraries - `npx playwright install-deps webkit`, needs root) makes
// that automatic; this suite skips WebKit with a clear message rather
// than failing when it isn't available, so it still runs everywhere else.

const { test, expect } = require('@playwright/test');
const { chromium, webkit } = require('playwright');
const { loadFixture } = require('../helpers');

/** @type {ReturnType<typeof loadFixture>} */
let fixture;

test.beforeAll(() => {
  fixture = loadFixture();
});

// `fixture.session_cookie` is the whole `name=value` pair (`e2e_harness.rs`
// takes it straight from the real login response's own `Set-Cookie` header,
// split on `;`) - `context.addCookies` wants just the *value* half; passing
// the whole `session=<token>` string as the value produces a real but
// wrong cookie and a real, hard-to-place failure (a 401 JSON body on the
// page, then every later `page.click()` call hanging for its own full
// timeout waiting for a selector that page will never have).
function sessionTokenValue() {
  return fixture.session_cookie.slice(fixture.session_cookie.indexOf('=') + 1);
}

// A spread of real device sizes (CSS px) covering iPhone, iPad, and two
// Android form factors (phone + tablet), each in both orientations -
// exactly the platforms named in the task this guards against regressing.
const DEVICE_SIZES = [
  { name: 'iPhone SE', w: 375, h: 667 },
  { name: 'iPhone 14', w: 390, h: 844 },
  { name: 'iPad', w: 768, h: 1024 },
  { name: 'iPad Pro', w: 1024, h: 1366 },
  { name: 'Pixel 7 (Android phone)', w: 412, h: 915 },
  { name: 'Galaxy S8 (Android phone)', w: 360, h: 740 },
  { name: 'Galaxy Tab (Android tablet)', w: 800, h: 1280 },
];

function withOrientations(sizes) {
  const out = [];
  for (const s of sizes) {
    out.push({ name: `${s.name} portrait`, w: s.w, h: s.h });
    out.push({ name: `${s.name} landscape`, w: s.h, h: s.w });
  }
  return out;
}

const EPSILON_PX = 1; // sub-pixel layout rounding, not a real overflow

async function fitMetrics(page) {
  return page.evaluate(() => ({
    scrollHeight: document.documentElement.scrollHeight,
    scrollWidth: document.documentElement.scrollWidth,
    innerHeight: window.innerHeight,
    innerWidth: window.innerWidth,
  }));
}

function assertFits(metrics, label) {
  const overflowY = metrics.scrollHeight - metrics.innerHeight;
  const overflowX = metrics.scrollWidth - metrics.innerWidth;
  expect(overflowY, `${label}: page needs vertical scrolling (scrollHeight ${metrics.scrollHeight} > innerHeight ${metrics.innerHeight})`).toBeLessThanOrEqual(EPSILON_PX);
  expect(overflowX, `${label}: page needs horizontal scrolling (scrollWidth ${metrics.scrollWidth} > innerWidth ${metrics.innerWidth})`).toBeLessThanOrEqual(EPSILON_PX);
}

// Drives one browser context through every screen state the real terminal
// has, asserting the fit requirement at each - the keypad on its own,
// after a real amount + note are entered (the tallest the keypad screen's
// own content gets), the charge/QR screen, and both the success and error
// confirmation overlay states (which carry the same note).
async function checkAllScreenStates(page, label) {
  // Deliberately no `waitUntil: 'networkidle'` - `pos.spec.js`'s own
  // proven-working navigation doesn't use it either, and this page's own
  // `/status/summary` health-check fetch can keep one request in flight
  // past whatever `networkidle`'s quiet-network window requires, hanging
  // the whole navigation (confirmed directly: every "fits" test timed out
  // at the full 10-minute test timeout with `networkidle`, every one
  // passed once switched to the default `load` wait).
  await page.goto(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/pos`);
  assertFits(await fitMetrics(page), `${label} / keypad`);

  for (const digit of ['1', '2', '3', '4', '5']) {
    await page.click(`.key[data-digit="${digit}"]`);
  }
  await page.fill('#note-input', 'A Fairly Long Customer Name Here');
  assertFits(await fitMetrics(page), `${label} / keypad with amount+note`);

  await page.click('#charge-btn');
  await page.waitForSelector('#payment-screen:not(.pos-screen-hidden)', { timeout: 15_000 });
  assertFits(await fitMetrics(page), `${label} / charge screen`);

  // The tick overlay only ever appears once a real payment is detected -
  // forced open directly here (this suite deliberately never sends a real
  // payment; `pos.spec.js` already proves that path for real) purely to
  // check its own layout fits, in both the plain-success and error visual
  // states, each carrying the same note (spec: shown on both).
  await page.evaluate(() => {
    document.getElementById('tick-overlay').classList.remove('pos-screen-hidden');
    document.getElementById('tick-note').textContent = 'A Fairly Long Customer Name Here';
  });
  assertFits(await fitMetrics(page), `${label} / confirmation overlay (success)`);

  await page.evaluate(() => {
    document.getElementById('tick-overlay').classList.add('is-error');
    document.getElementById('tick-error').textContent = 'Underpaid - the customer sent less than the requested amount.';
    document.getElementById('dismiss-btn').classList.remove('pos-screen-hidden');
  });
  assertFits(await fitMetrics(page), `${label} / confirmation overlay (error)`);
}

for (const size of withOrientations(DEVICE_SIZES)) {
  test(`fits with no scrolling on Chromium - ${size.name} (${size.w}x${size.h})`, async () => {
    test.setTimeout(30_000);
    const browser = await chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: size.w, height: size.h } });
      await context.addCookies([{ name: 'session', value: sessionTokenValue(), url: fixture.monokulo_base_url }]);
      const page = await context.newPage();
      await checkAllScreenStates(page, `chromium ${size.name}`);
    } finally {
      await browser.close();
    }
  });
}

let webkitAvailable = null;
async function isWebkitAvailable() {
  if (webkitAvailable !== null) return webkitAvailable;
  try {
    const browser = await webkit.launch();
    await browser.close();
    webkitAvailable = true;
  } catch (e) {
    webkitAvailable = false;
    console.log(`[pos-fit] WebKit not available in this environment (${e.message.split('\n')[0]}) - skipping WebKit checks. Run "npx playwright install-deps webkit" (needs root) to enable them.`);
  }
  return webkitAvailable;
}

for (const size of withOrientations(DEVICE_SIZES)) {
  test(`fits with no scrolling on WebKit (real Safari engine) - ${size.name} (${size.w}x${size.h})`, async () => {
    test.setTimeout(30_000);
    test.skip(!(await isWebkitAvailable()), 'WebKit is not installed/runnable in this environment');
    const browser = await webkit.launch();
    try {
      const context = await browser.newContext({ viewport: { width: size.w, height: size.h } });
      await context.addCookies([{ name: 'session', value: sessionTokenValue(), url: fixture.monokulo_base_url }]);
      const page = await context.newPage();
      await checkAllScreenStates(page, `webkit ${size.name}`);
    } finally {
      await browser.close();
    }
  });
}

// Regression guard for the *other* real bug found: resizing an
// already-loaded page (not just loading fresh at a new size) previously
// left the keypad stuck at the size computed for the old window, because
// `updateKeypadSize` measured its own already-rendered box instead of the
// real available space. Loads once at a generous size, then shrinks
// through several smaller ones without reloading, checking fit after each.
test('stays within bounds when the window is resized after the page has already loaded', async () => {
  test.setTimeout(30_000);
  const browser = await chromium.launch();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await context.addCookies([{ name: 'session', value: sessionTokenValue(), url: fixture.monokulo_base_url }]);
    const page = await context.newPage();
    await page.goto(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/pos`);

    const resizeSteps = [
      { width: 1024, height: 768 },
      { width: 800, height: 600 },
      { width: 390, height: 844 },
      { width: 844, height: 390 },
      { width: 1920, height: 1080 },
    ];
    for (const step of resizeSteps) {
      await page.setViewportSize(step);
      // Real layout settles asynchronously (the `resize` handler runs on
      // the next task) - poll briefly rather than a fixed sleep.
      await expect(async () => {
        const m = await fitMetrics(page);
        assertFits(m, `resized to ${step.width}x${step.height}`);
      }).toPass({ timeout: 2000 });
    }
  } finally {
    await browser.close();
  }
});

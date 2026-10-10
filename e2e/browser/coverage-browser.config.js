// @ts-check
const { spawnSync } = require('node:child_process');
const { defineConfig } = require('@playwright/test');
const realBinaries = require('./real-binaries.config.js');

// The POS's specs, which Firefox and WebKit (Safari's engine, and that of
// every browser on an iPad or iPhone) run too: a counter tablet runs
// whichever browser it came with.
const POS_FIXTURE_SPECS = ['pos-terminal.spec.js', 'pos-responsive-layout.spec.js', 'refund-camera.spec.js'];
const POS_REAL_SPECS = ['pos-session-diagnostics.spec.js'];

/**
 * Whether `browser` starts on this machine. On CI it must (the job installs
 * every browser and its system libraries), so it isn't asked: one that
 * can't start fails the run. Elsewhere, one missing its system libraries
 * (WebKit, off Debian and Ubuntu) leaves its projects out of the run, saying
 * so. Asked once per run: the workers inherit the answer with the
 * environment.
 */
function launches(browser) {
  if (process.env.CI) return true;
  const key = `MONOKULO_E2E_${browser.toUpperCase()}_LAUNCHES`;
  if (process.env[key] === undefined) {
    const probe = spawnSync(process.execPath, ['-e', `require('@playwright/test').${browser}.launch().then(b => b.close())`],
      { cwd: __dirname, encoding: 'utf8' });
    process.env[key] = probe.status === 0 ? '1' : '0';
    if (probe.status !== 0) {
      // The browser's own complaint, where it made one, says more than Playwright's.
      const reason = (probe.stderr.match(/\[err\] (.+)/) || probe.stderr.match(/browserType\.launch: (.+)/) || [, 'it would not start'])[1];
      console.warn(`${browser} can't start here (${reason.trim()}), so its POS projects are left out of this run; `
        + `"npx playwright install-deps ${browser}", as root, installs what it needs.`);
    }
  }
  return process.env[key] === '1';
}

const fixtureUse = { viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1, screenshot: 'only-on-failure', trace: 'retain-on-failure' };
const fixtureProject = (browser, testMatch) => ({
  testMatch,
  // Tests spread over workers one by one, not file by file: a file's
  // beforeAll fixture (coverage-fixture.js) then starts once per worker that
  // runs any of its tests, and its afterEach resets what a test changed.
  fullyParallel: true,
  use: { browserName: browser, ...fixtureUse },
  timeout: 40000,
});
const realBinariesProject = (browser, testMatch) => ({
  testMatch,
  // A spec file's tests share its processes and run in order.
  fullyParallel: realBinaries.fullyParallel,
  timeout: realBinaries.timeout,
  expect: realBinaries.expect,
  use: { ...realBinaries.use, browserName: browser, screenshot: 'only-on-failure', trace: 'retain-on-failure' },
});

// The browser coverage run (cargo xtask coverage browser): the deterministic
// browser tests against a fixture server and the real-binaries tests
// (real-binaries.config.js: the real engine and monokulo with a fake
// monerod) as projects of one run, sharing its workers, so none waits for
// another to finish. Chromium runs every test; Firefox and WebKit run the
// POS's. One HTML report and one JUnit report cover them all, and every
// browser's coverage is merged; the screenshot gallery is Chromium's.
module.exports = defineConfig({
  testDir: './tests',
  globalSetup: realBinaries.globalSetup,
  workers: realBinaries.workers,
  retries: 0,
  projects: [
    {
      name: 'fixture',
      ...fixtureProject('chromium', ['client-challenge-protocol.spec.js', 'checkout.spec.js', 'connection-challenge.spec.js', 'merchant-client.spec.js', ...POS_FIXTURE_SPECS, 'live-view.spec.js',
        'store-settings.spec.js', 'dropdowns.spec.js', 'order-creation.spec.js', 'javascript-disabled.spec.js', 'account-settings.spec.js', 'live-view-performance.spec.js', 'wallets-networks.spec.js', 'wallet-details.spec.js', 'disclosures.spec.js', 'short-values.spec.js', 'wallet-page.spec.js', 'store-site.spec.js', 'status-network-card.spec.js', 'store-webhooks.spec.js']),
      // The preexisting local surface badge case duplicates the real POS badge
      // test; keep the user's working edit without adding it to this suite.
      grepInvert: /POS uses the approved symbols in the compact stack and list badges/,
    },
    { name: 'real-binaries', ...realBinariesProject('chromium', realBinaries.testMatch) },
    ...['firefox', 'webkit'].filter(launches).flatMap(browser => [
      { name: `fixture-${browser}`, ...fixtureProject(browser, POS_FIXTURE_SPECS) },
      { name: `real-binaries-${browser}`, ...realBinariesProject(browser, POS_REAL_SPECS) },
    ]),
  ],
  reporter: [
    ['list'],
    ['html', { outputFolder: '../../target/coverage/browser/playwright-report', open: 'never' }],
    ['junit', { outputFile: '../../target/coverage/browser/junit.xml' }],
    ['./coverage-gallery-reporter.js', { required: ['checkout', 'pos', 'challenge', 'logs', 'pos-timeline', 'store-settings', 'site', 'hosted-payment', 'admin-settings', 'wallets', 'status'] }],
  ],
});

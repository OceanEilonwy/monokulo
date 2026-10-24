// @ts-check
const { defineConfig } = require('@playwright/test');
const realBinaries = require('./real-binaries.config.js');

const fixtureUse = { viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1, screenshot: 'only-on-failure', trace: 'retain-on-failure' };
const fixtureProject = (testMatch) => ({
  testMatch,
  // Tests spread over workers one by one, not file by file: a file's
  // beforeAll fixture (coverage-fixture.js) then starts once per worker that
  // runs any of its tests, and its afterEach resets what a test changed.
  fullyParallel: true,
  use: { browserName: 'chromium', ...fixtureUse },
  timeout: 40000,
});
const realBinariesProject = (testMatch) => ({
  testMatch,
  // A spec file's tests share its processes and run in order.
  fullyParallel: realBinaries.fullyParallel,
  timeout: realBinaries.timeout,
  expect: realBinaries.expect,
  use: { ...realBinaries.use, browserName: 'chromium', screenshot: 'only-on-failure', trace: 'retain-on-failure' },
});

// The browser coverage run (cargo xtask coverage browser): the deterministic
// browser tests against a fixture server and the real-binaries tests
// (real-binaries.config.js: the real engine and monokulo with a fake
// monerod) as projects of one run, sharing its workers, so none waits for
// another to finish. Chromium only: the counter tablet's browser and the
// Monokular app's WebKit go untested (decision 29 of the test audit). One
// HTML report and one JUnit report cover them all. The real-binaries
// project comes first: its files run their tests in order, the longest
// for most of the run, so a worker that starts one late finishes last.
module.exports = defineConfig({
  testDir: './tests',
  globalSetup: realBinaries.globalSetup,
  workers: realBinaries.workers,
  retries: 0,
  projects: [
    { name: 'real-binaries', ...realBinariesProject(realBinaries.testMatch) },
    {
      name: 'fixture',
      ...fixtureProject(['client-challenge-protocol.spec.js', 'client-onion.spec.js', 'client-path-prefix.spec.js', 'checkout.spec.js', 'connection-challenge.spec.js', 'merchant-client.spec.js', 'pos-terminal.spec.js', 'refund-camera.spec.js', 'live-view.spec.js',
        'store-settings.spec.js', 'domains.spec.js', 'dropdowns.spec.js', 'order-creation.spec.js', 'javascript-disabled.spec.js', 'account-settings.spec.js', 'live-view-performance.spec.js', 'wallets-networks.spec.js', 'wallet-details.spec.js', 'disclosures.spec.js', 'short-values.spec.js', 'wallet-page.spec.js', 'store-site.spec.js', 'status-network-card.spec.js', 'store-webhooks.spec.js', 'pages-gallery-themes.spec.js']),
    },
  ],
  reporter: [
    ['list'],
    ['html', { outputFolder: '../../target/coverage/browser/playwright-report', open: 'never' }],
    ['junit', { outputFile: '../../target/coverage/browser/junit.xml' }],
    ['./coverage-gallery-reporter.js', { required: ['checkout', 'pos', 'challenge', 'logs', 'pos-timeline', 'store-settings', 'site', 'hosted-payment', 'admin-settings', 'wallets', 'status'] }],
  ],
});

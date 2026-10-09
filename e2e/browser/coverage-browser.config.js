// @ts-check
const { defineConfig } = require('@playwright/test');
const realBinaries = require('./real-binaries.config.js');

// The browser coverage run (cargo xtask coverage browser): the deterministic
// browser tests against a fixture server and the real-binaries tests
// (real-binaries.config.js: the real engine and monokulo with a fake
// monerod) as two projects of one run, sharing its workers, so neither
// waits for the other to finish. One HTML report, one JUnit report and one
// screenshot gallery cover both.
module.exports = defineConfig({
  testDir: './tests',
  globalSetup: realBinaries.globalSetup,
  workers: realBinaries.workers,
  retries: 0,
  projects: [
    {
      name: 'fixture',
      testMatch: ['client-challenge-protocol.spec.js', 'checkout.spec.js', 'connection-challenge.spec.js', 'refund-camera.spec.js', 'merchant-client.spec.js', 'pos-terminal.spec.js', 'pos-responsive-layout.spec.js', 'live-view.spec.js',
        'store-settings.spec.js', 'dropdowns.spec.js', 'order-creation.spec.js', 'javascript-disabled.spec.js', 'account-settings.spec.js', 'live-view-performance.spec.js', 'wallets-networks.spec.js', 'wallet-details.spec.js'],
      // The preexisting local surface badge case duplicates the real POS badge
      // test; keep the user's working edit without adding it to this suite.
      grepInvert: /POS uses the approved symbols in the compact stack and list badges/,
      // Tests spread over workers one by one, not file by file: a file's
      // beforeAll fixture (coverage-fixture.js) then starts once per worker that
      // runs any of its tests, and its afterEach resets what a test changed.
      fullyParallel: true,
      use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1,
        screenshot: 'only-on-failure', trace: 'retain-on-failure' },
      timeout: 40000,
    },
    {
      name: 'real-binaries',
      testMatch: realBinaries.testMatch,
      // A spec file's tests share its processes and run in order.
      fullyParallel: realBinaries.fullyParallel,
      timeout: realBinaries.timeout,
      expect: realBinaries.expect,
      use: { ...realBinaries.use, screenshot: 'only-on-failure', trace: 'retain-on-failure' },
    },
  ],
  reporter: [
    ['list'],
    ['html', { outputFolder: '../../target/coverage/browser/playwright-report', open: 'never' }],
    ['junit', { outputFile: '../../target/coverage/browser/junit.xml' }],
    ['./coverage-gallery-reporter.js', { required: ['checkout', 'pos', 'challenge', 'logs', 'pos-timeline', 'store-settings', 'site', 'hosted-payment', 'admin-settings', 'wallets'] }],
  ],
});

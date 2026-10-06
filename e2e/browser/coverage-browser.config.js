const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  testMatch: ['client-challenge-protocol.spec.js', 'checkout.spec.js', 'connection-challenge.spec.js', 'refund-camera.spec.js', 'merchant-client.spec.js', 'pos-terminal.spec.js', 'pos-responsive-layout.spec.js', 'live-view.spec.js'],
  // The preexisting local surface badge case duplicates the real POS badge
  // test; keep the user's working edit without adding it to this suite.
  grepInvert: /POS uses the approved symbols in the compact stack and list badges/,
  // Tests spread over workers one by one, not file by file: a file's
  // beforeAll fixture (coverage-fixture.js) then starts once per worker that
  // runs any of its tests, and its afterEach resets what a test changed.
  fullyParallel: true,
  workers: 4,
  use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1,
    screenshot: 'only-on-failure', trace: 'retain-on-failure' },
  timeout: 40000,
  reporter: [['list'], ['html', { outputFolder: '../../target/coverage/browser/playwright-report', open: 'never' }], ['junit', { outputFile: '../../target/coverage/browser/junit-fixture.xml' }], ['./coverage-gallery-reporter.js']],
});

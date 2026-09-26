const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  testMatch: 'pos.spec.js',
  timeout: 10 * 60 * 1000,
  expect: { timeout: 60 * 1000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  globalSetup: require.resolve('./global-setup.js'),
  use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1,
    trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  reporter: [['list'], ['html', { outputFolder: '../../target/coverage/stagenet/playwright-report', open: 'never' }],
    ['./coverage-gallery-reporter.js']],
});

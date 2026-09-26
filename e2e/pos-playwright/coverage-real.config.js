const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  testMatch: ['coverage-checkout.spec.js', 'coverage-challenge.spec.js', 'coverage-pos.spec.js', 'coverage-fit.spec.js'],
  workers: 1,
  use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 },
  timeout: 30000,
  reporter: [['list']],
});

const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  testMatch: ['surface.spec.js', 'coverage-real.spec.js', 'coverage-checkout.spec.js', 'coverage-challenge.spec.js', 'coverage-pos.spec.js', 'coverage-fit.spec.js'],
  workers: 2,
  use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 },
  timeout: 40000,
  reporter: [['list']],
});

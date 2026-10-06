const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests',
  testMatch: ['checkout.spec.js', 'connection-challenge.spec.js', 'pos-terminal.spec.js', 'pos-responsive-layout.spec.js'],
  workers: 1,
  use: { browserName: 'chromium', viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 },
  timeout: 30000,
  reporter: [['list']],
});

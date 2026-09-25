// Fast browser tests for the checkout enhancements. No stagenet harness or
// wallet is needed; run with: npx playwright test -c surface.config.js.
const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  testDir: './tests',
  testMatch: 'surface.spec.js',
  use: { browserName: 'chromium' },
  timeout: 15000,
  reporter: [['list']],
});

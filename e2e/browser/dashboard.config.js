const { defineConfig } = require('@playwright/test');

// Local rendered-UI regressions for the dashboard, stores and live view.
module.exports = defineConfig({
  testDir: './tests',
  testMatch: [
    'store-setup.spec.js',
    'store-settings.spec.js',
    'dropdowns.spec.js',
    'order-creation.spec.js',
    'javascript-disabled.spec.js',
    'timezone-preferences.spec.js',
    'live-view-performance.spec.js',
    'live-view.spec.js',
  ],
  workers: 1,
  use: {
    browserName: 'chromium',
    viewport: { width: 1280, height: 900 },
    screenshot: 'only-on-failure',
  },
  timeout: 60000,
  reporter: [['list']],
  outputDir: '../../target/dashboard-browser',
});

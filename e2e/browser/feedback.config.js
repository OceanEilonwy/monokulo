const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  testDir: './tests', testMatch: ['feedback-*.spec.js', 'coverage-engine.spec.js'], workers: 1,
  use: { browserName: 'chromium', viewport: { width: 1280, height: 900 }, screenshot: 'only-on-failure' },
  timeout: 60000, reporter: [['list']], outputDir: '../../target/feedback-browser'
});

// @ts-check
const { defineConfig } = require('@playwright/test');
const realBinaries = require('./real-binaries.config.js');

// The real-binaries suite as part of the coverage run
// (scripts/coverage-browser.sh): the same tests against the real scanner and
// monokulo binaries, with their own HTML report, their stages added to the
// screenshot gallery the browser suite started, and - in the specs that use
// coverage-test.js - the instrumented browser assets and their coverage.
module.exports = defineConfig({
  ...realBinaries,
  reporter: [
    ['list'],
    ['html', { outputFolder: '../../target/coverage/browser/playwright-report-real', open: 'never' }],
    ['junit', { outputFile: '../../target/coverage/browser/junit-real-binaries.xml' }],
    ['./coverage-gallery-reporter.js', { required: ['logs', 'pos-timeline', 'store-settings', 'site', 'hosted-payment', 'admin-settings'], report: '../browser/playwright-report-real/index.html' }],
  ],
  use: { ...realBinaries.use, screenshot: 'only-on-failure', trace: 'retain-on-failure' },
});

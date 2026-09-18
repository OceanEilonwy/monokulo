// @ts-check
const { defineConfig } = require('@playwright/test');

// Real stagenet + real browser e2e for the POS terminal (templates/pos.html.hbs).
// See README.md for what this needs and how to run it - never wired into any
// default `npm test`/CI invocation anywhere else in this repo.
module.exports = defineConfig({
  testDir: './tests',
  // Real stagenet confirmations take real wall-clock time (~2min/block) - see
  // tests/pos.spec.js's own per-test test.setTimeout calls for the exact
  // budget each scenario gets (8min each); this is just a generous outer
  // bound in case a test omits its own override.
  timeout: 10 * 60 * 1000,
  expect: { timeout: 60 * 1000 },
  // Both tests share one real backend process and one real customer wallet
  // (see global-setup.js) - never run them concurrently against each other.
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  globalSetup: require.resolve('./global-setup.js'),
  use: {
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
});

// @ts-check
const { defineConfig } = require('@playwright/test');

// End-to-end tests against the real scanner and monokulo binaries and a
// local fake monerod (real-binaries-setup.js). Offline and quick; run with
// `npx playwright test -c real-binaries.config.js`. KEEP_E2E_LOGS=1 keeps
// the processes' logs and databases.
module.exports = defineConfig({
  testDir: './tests',
  testMatch: 'real-*.spec.js',
  timeout: 3 * 60 * 1000,
  expect: { timeout: 30 * 1000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  globalSetup: require.resolve('./real-binaries-setup.js'),
  use: { trace: 'retain-on-failure', screenshot: 'only-on-failure' },
});

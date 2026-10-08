// @ts-check
const { defineConfig } = require('@playwright/test');

// End-to-end tests against the real engine and monokulo binaries and a
// local fake monerod (real-stack.js). Offline and quick; run with
// `npx playwright test -c real-binaries.config.js`. KEEP_E2E_LOGS=1 keeps
// the processes' logs and databases.
module.exports = defineConfig({
  testDir: './tests',
  testMatch: [
    'node-settings.spec.js',
    'store-key-custody.spec.js',
    'admin-settings-layout.spec.js',
    'engine-crash-recovery.spec.js',
    'log-search-and-tracing.spec.js',
    'settings-section-updates.spec.js',
    'pos-session-diagnostics.spec.js',
    'site-themes.spec.js',
    'admin-settings-and-nodes.spec.js',
    'wallet-setup.spec.js',
  ],
  timeout: 3 * 60 * 1000,
  expect: { timeout: 30 * 1000 },
  // A spec file's tests share its processes and run in order; files each
  // have their own (tests/backend-helpers.js useRealStack), so they run side
  // by side. Most of a file's time is spent waiting on the engine and
  // monokulo, not the CPU, hence more workers than Playwright's default.
  fullyParallel: false,
  workers: process.env.E2E_WORKERS ? Number(process.env.E2E_WORKERS) : 4,
  retries: 0,
  reporter: [['list']],
  globalSetup: require.resolve('./real-binaries-setup.js'),
  use: { trace: 'retain-on-failure', screenshot: 'only-on-failure' },
});

import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './browser',
  // Every spec opens its own page on the shared servers: none waits for
  // another file's.
  fullyParallel: true,
  outputDir: fileURLToPath(new URL('../../test-results/ui', import.meta.url)),
  use: { baseURL: 'http://127.0.0.1:4179', browserName: 'chromium' },
  webServer: [{
    cwd: fileURLToPath(new URL('../../', import.meta.url)),
    command: 'npm run build:stylebook && npm run preview -w @monokulo/stylebook -- --port 4179 --strictPort',
    url: 'http://127.0.0.1:4179', reuseExistingServer: !process.env.CI,
  }, {
    cwd: fileURLToPath(new URL('../../', import.meta.url)),
    command: 'npx vite --config crates/ui/browser/solid/vite.config.js --host 127.0.0.1 --port 4181 --strictPort',
    url: 'http://127.0.0.1:4181', reuseExistingServer: !process.env.CI,
  }],
});

import { test, expect } from '@playwright/test';

// Every example of every component in crates/ui, rendered through the
// generated Solid component, must give the DOM the Rust component renders.
// `cargo xtask ui` writes the components and the fixtures; see crates/ui.
test('the Solid components render what the Rust components render', async ({ page }) => {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('http://127.0.0.1:4181/');
  const results = await page.waitForFunction(() => window.equivalence).then(handle => handle.jsonValue());
  expect(errors).toEqual([]);
  expect(results.length).toBeGreaterThan(0);
  for (const result of results) {
    expect.soft(result.solid, `${result.component}: ${result.title}`).toBe(result.rust);
  }
});

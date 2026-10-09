// <mk-select>, the site's one dropdown (docs/dropdowns.md): drawn over the
// page's own select, used with the mouse and the keyboard, and the plain
// select without JavaScript.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
let fixture;
// This file's tests share one fixture and the settings they save, so
// they run in order on one worker.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const store = '/dashboard/stores/coverage-store';

test('a dropdown floats over the page, picks with the mouse and saves through the real select', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + store + '/settings');
  const section = page.locator('#base-currency');
  await section.getByRole('button', { name: 'Edit base currency', exact: true }).click();
  const dropdown = section.locator('mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Base currency' });
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute('aria-expanded', 'false');

  // Opening it moves nothing: the list is drawn over what's below.
  const help = section.locator('dialog .field-help').first();
  const before = await help.boundingBox();
  await button.click();
  const list = dropdown.getByRole('listbox');
  await expect(list).toBeVisible();
  await expect(button).toHaveAttribute('aria-expanded', 'true');
  expect(await help.boundingBox()).toEqual(before);
  const listBox = await list.boundingBox();
  expect(listBox.y).toBeGreaterThan((await button.boundingBox()).y);

  // An option shows its name and its code as a separate part.
  const usd = list.getByRole('option', { name: /United States Dollar/ });
  await expect(usd.locator('.mk-detail')).toHaveText('USD');
  await usd.click();
  await expect(list).not.toBeVisible();
  await expect(button).toBeFocused();
  await expect(dropdown.locator('select')).toHaveValue('USD');
  await expect(button.locator('.mk-detail')).toHaveText('USD');

  await section.getByRole('button', { name: 'Update', exact: true }).click();
  await expect(section).toContainText('Settings saved.');
  await expect(section.locator('.settings-summary')).toHaveText('USD');
  expect(errors).toEqual([]);
});

test('the keyboard opens, moves, chooses and closes without leaving the dialog', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + store + '/settings');
  const section = page.locator('#base-currency');
  await section.getByRole('button', { name: 'Edit base currency', exact: true }).click();
  const dropdown = section.locator('mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Base currency' });
  const find = dropdown.getByRole('combobox', { name: 'Find' });
  const select = dropdown.locator('select');
  const start = await select.inputValue();
  await button.focus();

  // Escape closes the list only: the dialog stays, nothing changes.
  await page.keyboard.press('ArrowDown');
  await expect(dropdown.getByRole('listbox')).toBeVisible();
  await expect(find).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dropdown.getByRole('listbox')).not.toBeVisible();
  await expect(section.locator('dialog')).toBeVisible();
  await expect(button).toBeFocused();
  await expect(select).toHaveValue(start);

  // Opens on the chosen option; Down then Enter takes the next one.
  await page.keyboard.press('Enter');
  const active = dropdown.locator('.mk-option.mk-active');
  await expect(active).toHaveAttribute('aria-selected', 'true');
  await expect(find).toHaveAttribute('aria-activedescendant', await active.getAttribute('id'));
  await expect(active).toBeInViewport();
  await page.keyboard.press('ArrowDown');
  const next = await dropdown.locator('.mk-option.mk-active').getAttribute('data-index');
  await page.keyboard.press('Enter');
  await expect(dropdown.getByRole('listbox')).not.toBeVisible();
  await expect(button).toBeFocused();
  await expect.poll(() => select.evaluate(s => String(s.selectedIndex))).toBe(next);
  await expect(select).not.toHaveValue(start);

  // Typing on the button starts a search.
  await page.keyboard.type('eur');
  await expect(find).toHaveValue('eur');
  await page.keyboard.press('Enter');
  await expect(select).toHaveValue('EUR');
});

test('a long list has a find box, and a script setting the select is followed', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/dashboard');
  await page.locator('#timezone summary').click();
  const dropdown = page.locator('#timezone mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Show dates and times in' });
  await button.click();
  const find = dropdown.getByRole('combobox', { name: 'Find' });
  await expect(find).toBeFocused();
  await find.fill('perth');
  await expect(dropdown.getByRole('option')).toHaveCount(1);
  await page.keyboard.press('Enter');
  await expect(button).toContainText('Australia/Perth');
  await expect(dropdown.locator('select')).toHaveValue('Australia/Perth');

  await button.click();
  await dropdown.getByRole('combobox', { name: 'Find' }).fill('no such place');
  await expect(dropdown.getByRole('option')).toHaveCount(0);
  await expect(dropdown.getByText('Nothing matches.')).toBeVisible();
  await page.keyboard.press('Escape');

  // The select is still the field: a script that sets it shows on the button.
  await dropdown.locator('select').evaluate(s => { s.value = 'Europe/London'; });
  await expect(button).toContainText('Europe/London');

  await page.locator('#timezone').getByRole('button', { name: 'Save' }).click();
  await expect(page.locator('.timezone-current')).toContainText('Europe/London');
});

test('search is shown, hidden or decided by the number of options', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/dashboard');
  const find = await page.evaluate(async () => {
    const make = (count, search) => {
      const element = document.createElement('mk-select');
      if (search) element.setAttribute('search', search);
      const select = document.createElement('select');
      select.setAttribute('aria-label', `${count} ${search || 'auto'}`);
      for (let i = 0; i < count; i++) select.add(new Option(`Option ${i}`, String(i)));
      element.append(select);
      document.body.append(element);
      element.querySelector('.mk-button').click();
      const shown = !!element.querySelector('.mk-find');
      element.remove();
      return shown;
    };
    return [make(11), make(12), make(3, 'show'), make(40, 'hide')];
  });
  expect(find).toEqual([false, true, true, false]);
});

test('a required dropdown left on its prompt is refused, and green is only for Current', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/dashboard');
  await page.evaluate(() => {
    const form = document.createElement('form');
    form.id = 'test-form';
    form.innerHTML = `<label>Wallet <mk-select><select name="wallet_id" required>
      <option value="" disabled selected>Choose a wallet…</option>
      <option value="w1" data-label="Copper Heron" data-detail="48xQ…v3Rk" data-chip="Current" data-chip-tone="current" data-note="since 1 Sep">Copper Heron</option>
      <option value="w2" data-label="Café till" data-chip="Kept here">Café till</option>
      <option value="w3" disabled data-label="Quiet Lantern" data-note="stagenet">Quiet Lantern</option>
    </select></mk-select></label><button>Go</button>`;
    form.addEventListener('submit', event => event.preventDefault());
    document.body.prepend(form);
  });
  const dropdown = page.locator('#test-form mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Wallet' });
  await expect(button.locator('.mk-prompt')).toHaveText('Choose a wallet…');
  await page.locator('#test-form button:not(.mk-button)').click();
  await expect(dropdown).toHaveClass(/mk-invalid/);
  await expect(button).toHaveAttribute('aria-invalid', 'true');
  await expect(button).toBeFocused();

  await button.click();
  await expect(dropdown.getByRole('option')).toHaveCount(3);
  await expect(dropdown.getByRole('option', { name: /Copper Heron/ }).locator('.tag-ok')).toHaveText('Current');
  await expect(dropdown.getByRole('option', { name: /Café till/ }).locator('.tag-unknown')).toHaveText('Kept here');
  const off = dropdown.getByRole('option', { name: /Quiet Lantern/ });
  await expect(off).toHaveAttribute('aria-disabled', 'true');
  await off.click();
  await expect(dropdown.getByRole('listbox')).toBeVisible();
  await expect(dropdown.locator('select')).toHaveValue('');
  await dropdown.getByRole('option', { name: /Copper Heron/ }).click();
  await expect(dropdown).not.toHaveClass(/mk-invalid/);
  await expect(button.locator('.mk-label')).toHaveText('Copper Heron');

  // A short list: no find box; Home, End and typing move over the options
  // that can be picked.
  await button.focus();
  await page.keyboard.press('ArrowDown');
  await expect(dropdown.locator('.mk-find')).toHaveCount(0);
  await page.keyboard.press('End');
  await expect(dropdown.locator('.mk-active')).toContainText('Café till');
  await page.keyboard.press('Home');
  await expect(dropdown.locator('.mk-active')).toContainText('Copper Heron');
  await page.keyboard.type('caf');
  await expect(dropdown.locator('.mk-active')).toContainText('Café till');
  await page.keyboard.press(' ');
  await expect(dropdown.locator('select')).toHaveValue('w2');
});

test('without JavaScript the dropdown is the plain select, every part in words', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + store + '/settings');
  const select = page.locator('#base-currency select[name=base_currency]');
  await expect(page.locator('#base-currency .mk-button')).toHaveCount(0);
  await expect(select.locator('option[value=USD]')).toHaveText('United States Dollar (USD)');
  await context.close();
});

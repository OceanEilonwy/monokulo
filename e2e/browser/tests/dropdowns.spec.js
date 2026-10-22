// The Select (crates/ui), the site's one dropdown: drawn over the page's
// own select once its island attaches, used with the mouse and the
// keyboard, and the plain select without JavaScript.
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

// The markup `ui::Select` renders around a select (crates/ui): its root is
// an island the page's islands script attaches its behaviour to. Only
// `search` is read from the props once the select is on the page.
const ISLAND = (select, search = 'auto') =>
  `<div class="mk-select" data-mk="1" data-island="Select" data-props='{"search":"${search}"}'>` +
  `${select.replace('<select', '<select class="mk-input" data-mk="2"')}<!--mk:3--></div>`;

test('a dropdown floats over the page, picks with the mouse and saves through the real select', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + store + '/settings');
  const section = page.locator('#card-base-currency');
  // At the top of the window, so the list has room to open below.
  await section.evaluate((card) => card.scrollIntoView({ block: 'start' }));
  const dropdown = section.locator('.mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Base currency' });
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute('aria-expanded', 'false');

  // Opening it moves nothing: the list is drawn over what's below.
  const help = section.locator('.field-help').first();
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

  await page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Settings saved');
  await expect(section.locator('select')).toHaveValue('USD');
  await expect(section.locator('[data-card-saved]')).toHaveText('Saved');
  expect(errors).toEqual([]);
});

test('the keyboard opens, moves, chooses and closes without leaving the card', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + store + '/settings');
  const section = page.locator('#card-base-currency');
  const dropdown = section.locator('.mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Base currency' });
  const find = dropdown.getByRole('combobox', { name: 'Find' });
  const select = dropdown.locator('select');
  const start = await select.inputValue();
  await button.focus();

  // Escape closes the list only: nothing changes.
  await page.keyboard.press('ArrowDown');
  await expect(dropdown.getByRole('listbox')).toBeVisible();
  await expect(find).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dropdown.getByRole('listbox')).not.toBeVisible();
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
  await page.goto(fixture.base_url + '/account');
  const dropdown = page.locator('#card-time .mk-select').first();
  const button = dropdown.getByRole('combobox', { name: 'Time zone' });
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

  // The select is still the field: a script that sets it, and says so with
  // an event, shows on the button.
  await dropdown.locator('select').evaluate(s => {
    s.value = 'Europe/London';
    s.dispatchEvent(new Event('change', { bubbles: true }));
  });
  await expect(button).toContainText('Europe/London');

  await page.locator('#save-bar').getByRole('button', { name: 'Save' }).click();
  await expect(page.locator('#timezone')).toHaveValue('Europe/London');
  await expect(page.locator('.acct-menu')).toContainText('Times in Europe/London');
});

test('search is shown, hidden or decided by the number of options', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/');
  const find = await page.evaluate(async (island) => {
    // An island attaches once its behaviour has loaded: the first time, a
    // fetch.
    const attached = async (holder) => {
      for (let tries = 0; tries < 500 && !holder.querySelector('.mk-button'); tries++) {
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      return holder.querySelector('.mk-button');
    };
    const make = async (count, search) => {
      const options = Array.from({ length: count }, (_, i) => `<option value="${i}">Option ${i}</option>`).join('');
      const holder = document.createElement('div');
      holder.innerHTML = eval(island)(`<select aria-label="${count} ${search}">${options}</select>`, search);
      document.body.append(holder);
      (await attached(holder)).click();
      await new Promise(resolve => setTimeout(resolve));
      const shown = !!holder.querySelector('.mk-find');
      holder.remove();
      return shown;
    };
    return [await make(11, 'auto'), await make(12, 'auto'), await make(3, 'show'), await make(40, 'hide')];
  }, ISLAND.toString());
  expect(find).toEqual([false, true, true, false]);
});

test('a required dropdown left on its prompt is refused, and green is only for Current', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/');
  await page.evaluate((source) => {
    const island = eval(source);
    const form = document.createElement('form');
    form.id = 'test-form';
    form.innerHTML = `<label>Wallet ${island(`<select name="wallet_id" required>
      <option value="" disabled selected>Choose a wallet…</option>
      <option value="w1" data-label="Copper Heron" data-detail="48xQ…v3Rk" data-chip="Current" data-chip-tone="current" data-note="since 1 Sep">Copper Heron</option>
      <option value="w2" data-label="Café till" data-chip="Kept here">Café till</option>
      <option value="w3" disabled data-label="Quiet Lantern" data-note="stagenet">Quiet Lantern</option>
    </select>`)}</label><button>Go</button>`;
    form.addEventListener('submit', event => event.preventDefault());
    document.body.prepend(form);
  }, ISLAND.toString());
  const dropdown = page.locator('#test-form .mk-select');
  const button = dropdown.getByRole('combobox', { name: 'Wallet' });
  await expect(button.locator('.mk-prompt')).toHaveText('Choose a wallet…');
  await page.locator('#test-form button:not(.mk-button)').click();
  await expect(button).toHaveAttribute('aria-invalid', 'true');
  await expect(button).toBeFocused();

  await button.click();
  await expect(dropdown.getByRole('option')).toHaveCount(3);
  await expect(dropdown.getByRole('option', { name: /Copper Heron/ }).locator('.mk-current')).toHaveText('Current');
  await expect(dropdown.getByRole('option', { name: /Café till/ }).locator('.tag-unknown')).toHaveText('Kept here');
  const off = dropdown.getByRole('option', { name: /Quiet Lantern/ });
  await expect(off).toHaveAttribute('aria-disabled', 'true');
  await off.click();
  await expect(dropdown.getByRole('listbox')).toBeVisible();
  await expect(dropdown.locator('select')).toHaveValue('');
  await dropdown.getByRole('option', { name: /Copper Heron/ }).click();
  await expect(button).not.toHaveAttribute('aria-invalid');
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
  const select = page.locator('#card-base-currency select[name=base_currency]');
  await expect(page.locator('#card-base-currency .mk-button')).toHaveCount(0);
  await expect(select.locator('option[value=USD]')).toHaveText('United States Dollar (USD)');
  await context.close();
});

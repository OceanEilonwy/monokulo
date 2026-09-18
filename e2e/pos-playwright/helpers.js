const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const FIXTURE_PATH = path.join(__dirname, '.pos-e2e-fixture.json');
const SEND_PAYMENT_BIN = path.join(REPO_ROOT, 'target', 'debug', 'pos-e2e-send-payment');

function loadFixture() {
  return JSON.parse(fs.readFileSync(FIXTURE_PATH, 'utf8'));
}

// `xmr_amount` is always monokulo's own fixed-12-decimal display string
// (`shared::exchange_rate::format_piconero_as_xmr`, e.g. "0.000335000000") -
// BigInt arithmetic on the split string, never `parseFloat`, so this can
// never lose or misround the exact integer piconero amount the real payment
// has to match.
function piconeroFromXmrDisplay(display) {
  const [whole, frac] = display.split('.');
  return BigInt(whole) * 1_000_000_000_000n + BigInt(frac);
}

// Shells out to the real Rust binary that connects, signs, and broadcasts an
// actual stagenet transaction (`scanner::e2e_wallet::StagenetSpendWallet`,
// via `crates/scanner/src/bin/pos_e2e_send_payment.rs`) - deliberately never
// reimplemented in JS; this process never touches key material itself.
// Returns the broadcast tx's hex-encoded hash.
function sendStagenetPayment(address, piconero) {
  return execFileSync(SEND_PAYMENT_BIN, [address, piconero.toString()], { cwd: REPO_ROOT, encoding: 'utf8' }).trim();
}

// Square-Terminal-style keypad entry: taps each character of `digits` (e.g.
// "335000000") on the real POS page's own numeric keys, in order - the exact
// same sequence of clicks a merchant would make, not a shortcut around the
// real digit-shift-from-the-right UI (`templates/pos.html.hbs`).
async function enterAmount(page, digits) {
  for (const digit of digits) {
    await page.click(`.key[data-digit="${digit}"]`);
  }
}

module.exports = { loadFixture, piconeroFromXmrDisplay, sendStagenetPayment, enterAmount };

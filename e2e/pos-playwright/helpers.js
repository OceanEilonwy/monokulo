const fs = require('node:fs');
const path = require('node:path');

const FIXTURE_PATH = path.join(__dirname, '.pos-e2e-fixture.json');

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

// Calls the real, in-process "send a payment" endpoint e2e-harness itself
// exposes (`fixture.send_payment_url`, `send_payment_handler` in
// crates/scanner/src/bin/e2e_harness.rs) - which connects, signs, and
// broadcasts an actual stagenet transaction via `cli_wallet::
// StagenetTestWallet`, deliberately never reimplemented in JS; this Node
// process never touches key material itself.
//
// Not a separate child process - a real, reproduced failure ruled that out:
// the public stagenet node (or something in front of it) allows only one
// concurrent connection per source IP, so a second process's own connection
// attempt, racing against e2e-harness's own background scan loop, failed
// *consistently* (not flakily) for the scan loop's entire lifetime. Routing
// the send through e2e-harness's own process instead lets it serialize this
// against its own scan loop with a single in-process lock - see
// `network_lock`'s own doc comment on the Rust side for the full story.
async function sendStagenetPayment(sendPaymentUrl, address, piconero) {
  const response = await fetch(sendPaymentUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    // JSON has no BigInt literal - `piconero` arrives here as a BigInt
    // (see piconeroFromXmrDisplay), so it's sent as a plain numeral string
    // and parsed with serde's own u64 support on the Rust side, never
    // round-tripped through a JS `number` (which can't hold it exactly
    // past 2^53).
    body: JSON.stringify({ address, piconero_amount: piconero.toString() }),
  });
  if (!response.ok) {
    throw new Error(`send-payment failed (${response.status}): ${await response.text()}`);
  }
  const body = await response.json();
  return body.tx_hash;
}

// Square-Terminal-style keypad entry: taps each character of `digits` (e.g.
// "335000000") on the real POS page's own numeric keys, in order - the exact
// same sequence of clicks a merchant would make, not a shortcut around the
// real digit-shift-from-the-right UI (`templates/pos.html.hbs`).
async function enterAmount(page, digits) {
  for (const digit of digits) {
    await page.getByRole('button', { name: digit, exact: true }).click();
  }
}

module.exports = { loadFixture, piconeroFromXmrDisplay, sendStagenetPayment, enterAmount };

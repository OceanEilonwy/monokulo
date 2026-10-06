// @ts-check
// Crash safety with a real process (admin_settings_v2.md task 7.11): the
// engine binary is killed with SIGKILL at random moments while orders are
// being created and its scan loop is ticking, then started again on the
// same database. Every order it confirmed must still be there, no two
// orders may share an address, and it must come back healthy. (Killing a
// task inside one process can't show what SQLite does when the process
// really dies.) Payments can't be made against the fake node, so payment
// recording under crashes is covered by the in-process crash-injection
// test in the engine crate.
const { test, expect } = require('@playwright/test');
const { spawn } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const { useRealStack, fixture, VIEW_KEY, SPEND_PUBKEY } = require('./backend-helpers');

useRealStack(test);

const ENGINE_BIN = process.env.E2E_ENGINE_BIN || path.resolve(__dirname, '..', '..', '..', 'target', 'debug', 'monokulo-engine');

function freePort() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

test('killing the engine at random moments never loses a confirmed order or reuses an address', async () => {
  test.setTimeout(120_000);
  const { logs, fake_monerod: fakeAddress } = fixture();
  const [fakeHost, fakePort] = fakeAddress.split(':');
  const port = await freePort();
  const url = `http://127.0.0.1:${port}`;
  const token = crypto.randomBytes(16).toString('hex');
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('ENGINE_')));
  const env = { ...inherited, ENGINE_TOKEN: token };
  // Its own options file, which the settings saved below go to, so they
  // hold across every restart.
  const options = path.join(logs, 'crash.toml');
  fs.writeFileSync(options, `[server]\nbind = "127.0.0.1:${port}"\n[database]\npath = ${JSON.stringify(path.join(logs, 'crash.db'))}\n`);
  // The engine answers nothing without the engine token, so every request
  // carries it, as monokulo's do.
  const engineFetch = (pathAndQuery, init = {}) =>
    fetch(`${url}${pathAndQuery}`, { ...init, headers: { 'x-engine-token': token, ...(init.headers || {}) } });

  const kill = (child) => new Promise((resolve) => {
    if (!child || child.exitCode !== null || child.signalCode !== null) return resolve(undefined);
    child.once('exit', resolve);
    child.kill('SIGKILL');
  });
  let engine = null;
  const start = async () => {
    engine = spawn(ENGINE_BIN, ['--options', options], { env, stdio: 'ignore' });
    await expect.poll(async () => {
      try { return (await engineFetch(`/status`)).status; } catch { return 0; }
    }, { timeout: 20_000, intervals: [100] }).toBe(200);
  };

  try {
  await start();
  const saved = await engineFetch(`/api/v1/admin/settings`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      scalars: { 'payment.mempool_poll_interval_ms': '100', 'server.rate_limit_per_token_per_min': '100000' },
      monero_node: { stagenet: { host: fakeHost, port: Number(fakePort), ssl: false, accept_self_signed_certs: true, fallbacks: [] } },
    }),
  });
  expect(saved.status).toBe(200);
  // The store is created once the engine has recorded the fake node's tip,
  // so the store starts there. Created before, it would start a block
  // behind, and the fake node can't serve blocks to scan (it has no
  // get_blocks.bin): every tick would then fail, on a slow machine.
  const scanned = async () => {
    const status = await (await engineFetch(`/status`)).json();
    const stagenet = status.networks.find((n) => n.network === 'stagenet');
    return Boolean(stagenet && stagenet.scanner.last_tick_ok);
  };
  await expect.poll(scanned, { timeout: 20_000 }).toBe(true);
  const created = await (await engineFetch(`/api/v1/admin/tenants`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ view_key_hex: VIEW_KEY, spend_pubkey_hex: SPEND_PUBKEY, network: 'stagenet' }),
  })).json();
  const sk = created.secret_token;

  const confirmed = new Map(); // order id -> address, for every 200 the engine sent
  for (let round = 0; round < 6; round++) {
    let running = true;
    const makeOrders = (async () => {
      while (running) {
        try {
          const response = await engineFetch(`/api/v1/admin/tenant/orders`, {
            method: 'POST',
            headers: { authorization: `Bearer ${sk}`, 'content-type': 'application/json' },
            body: JSON.stringify({ xmr_amount_piconero: 1000 + round }),
          });
          if (response.status === 200) {
            const order = await response.json();
            confirmed.set(order.order_id, order.address);
          }
        } catch {
          // The engine is being killed; requests in flight fail.
        }
      }
    })();
    await new Promise((r) => setTimeout(r, 150 + Math.random() * 600));
    await kill(engine);
    running = false;
    await makeOrders;
    await start();
  }

    expect(confirmed.size).toBeGreaterThan(10);
    // Every confirmed order survived, with the address it was given.
    for (const [orderId, address] of confirmed) {
      const response = await engineFetch(`/api/v1/admin/tenant/orders/${orderId}`, { headers: { authorization: `Bearer ${sk}` } });
      expect(response.status, `order ${orderId} lost`).toBe(200);
      expect((await response.json()).address).toBe(address);
    }
    // No address was handed out twice, across every restart - counting
    // every order the engine holds, including any it committed just before
    // a kill whose response never arrived.
    const all = [];
    for (let offset = 0; ; offset += 200) {
      const page = await (await engineFetch(`/api/v1/admin/tenant/orders?limit=200&offset=${offset}`, { headers: { authorization: `Bearer ${sk}` } })).json();
      all.push(...page);
      if (page.length < 200) break;
    }
    expect(all.length).toBeGreaterThanOrEqual(confirmed.size);
    expect(new Set(all.map((order) => order.address)).size).toBe(all.length);
    // And it's scanning again.
    await expect.poll(scanned, { timeout: 20_000 }).toBe(true);
  } finally {
    await kill(engine);
  }
});

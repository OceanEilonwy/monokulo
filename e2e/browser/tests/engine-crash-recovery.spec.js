// @ts-check
// Crash safety with a real process (admin_settings_v2.md task 7.11): the
// engine binary is killed with SIGKILL at random moments while orders are
// being created and its scan loop is ticking, then started again on the
// same database. Every order it confirmed must still be there, no two
// orders may share an address, and it must come back healthy. (Killing a
// task inside one process can't show what SQLite does when the process
// really dies.) Payments can't be made against the fake node, so payment
// recording under crashes is covered by the in-process crash-injection
// test in the engine crate. The kill moments are drawn from a seed, printed
// and named in every failure: E2E_CRASH_SEED=<seed> replays them.
const { test, expect } = require('@playwright/test');
const { spawn } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { useRealStack, fixture, VIEW_KEY, SPEND_PUBKEY } = require('./backend-helpers');
const { readyLine, tee } = require('../real-stack');

useRealStack(test);

const ENGINE_BIN = process.env.E2E_ENGINE_BIN || path.resolve(__dirname, '..', '..', '..', 'target', 'debug', 'monokulo-engine');

// mulberry32: a small seeded generator of numbers in [0, 1), so a run's
// kill moments can be drawn again from its seed.
function seeded(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

test('killing the engine at random moments never loses a confirmed order or reuses an address', async () => {
  test.setTimeout(120_000);
  const { logs, fake_monerod: fakeAddress } = fixture();
  const [fakeHost, fakePort] = fakeAddress.split(':');
  const seed = Number(process.env.E2E_CRASH_SEED) || (Date.now() >>> 0);
  console.log(`[engine-crash-recovery] kill moments from seed ${seed} (E2E_CRASH_SEED=${seed} replays them)`);
  const random = seeded(seed);
  const delays = [];
  // What replays this run, for every failure message.
  const replay = () => `(seed ${seed}, kills after ${delays.join(', ') || 'none yet'} ms)`;
  const token = crypto.randomBytes(16).toString('hex');
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('ENGINE_')));
  const env = { ...inherited, ENGINE_TOKEN: token };
  // Its own options file, which the settings saved below go to, so they
  // hold across every restart. Any free port: each process says which
  // (ENGINE_LISTENING), and requests go to the one running.
  const options = path.join(logs, 'crash.toml');
  fs.writeFileSync(options, `[server]\nbind = "127.0.0.1:0"\n[database]\npath = ${JSON.stringify(path.join(logs, 'crash.db'))}\n`);
  let url = null;
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
    engine = spawn(ENGINE_BIN, ['--options', options], { env, stdio: ['ignore', 'pipe', 'ignore'] });
    tee(engine.stdout, path.join(logs, 'crash-engine.log'));
    url = `http://${await readyLine(engine, engine.stdout, 'ENGINE_LISTENING ', {
      timeout: 20_000,
      hint: process.env.E2E_ENGINE_BIN ? `; an E2E_ENGINE_BIN built before the engine printed it doesn't` : '',
    })}`;
    await expect.poll(async () => {
      try { return (await engineFetch(`/status`)).status; } catch { return 0; }
    }, { message: `the engine came back healthy ${replay()}`, timeout: 20_000, intervals: [100] }).toBe(200);
  };

  try {
  await start();
  const saved = await engineFetch(`/api/v1/admin/settings`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      scalars: { 'payment.mempool_poll_interval_ms': '100', 'server.rate_limit_per_token_per_min': '100000', 'server.instance_rate_limit_per_min': '100000' },
      monero_node: { stagenet: { host: fakeHost, port: Number(fakePort), ssl: false, accept_self_signed_certs: true, fallbacks: [] } },
    }),
  });
  expect(saved.status, `settings saved ${replay()}`).toBe(200);
  // The store is created once the engine has recorded the fake node's tip,
  // so the store starts there. Created before, it would start a block
  // behind, and the fake node can't serve blocks to scan (it has no
  // get_blocks.bin): every tick would then fail, on a slow machine.
  const scanned = async () => {
    const status = await (await engineFetch(`/status`)).json();
    const stagenet = status.networks.find((n) => n.network === 'stagenet');
    return Boolean(stagenet && stagenet.scanner.last_tick_ok);
  };
  await expect.poll(scanned, { message: `scanning before the kills ${replay()}`, timeout: 20_000 }).toBe(true);
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
    const delay = Math.round(150 + random() * 600);
    delays.push(delay);
    await new Promise((r) => setTimeout(r, delay));
    await kill(engine);
    running = false;
    await makeOrders;
    await start();
  }

    expect(confirmed.size, `orders confirmed ${replay()}`).toBeGreaterThan(10);
    // Every confirmed order survived, with the address it was given.
    for (const [orderId, address] of confirmed) {
      const response = await engineFetch(`/api/v1/admin/tenant/orders/${orderId}`, { headers: { authorization: `Bearer ${sk}` } });
      expect(response.status, `order ${orderId} lost ${replay()}`).toBe(200);
      expect((await response.json()).address, `order ${orderId}'s address changed ${replay()}`).toBe(address);
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
    expect(all.length, `orders held ${replay()}`).toBeGreaterThanOrEqual(confirmed.size);
    expect(new Set(all.map((order) => order.address)).size, `an address reused ${replay()}`).toBe(all.length);
    // And it's scanning again.
    await expect.poll(scanned, { message: `scanning again after the kills ${replay()}`, timeout: 20_000 }).toBe(true);
  } finally {
    await kill(engine);
  }
});

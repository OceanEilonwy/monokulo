# Announcements from the node (ZMQ)

Status: implemented behind the engine's `zmq` cargo feature, which the
Docker image turns on (a plain `cargo build` leaves it off).

The engine learns about blocks and pool transactions by polling `monerod`
(`docs/node_rpc_efficiency.md`): a round every `payment.mempool_poll_interval_ms`
(1 s by default) and a fast pool pass every 250 ms. monerod can also announce
both over ZeroMQ (`--zmq-pub`). With the feature on and a node's `zmq_pub`
set, an announcement ends the loop's wait at once, so a payment is seen
within a round trip of the node seeing it rather than within a poll interval.

## Why it is possible now

The design ruled ZMQ out (`docs/DESIGN.md` §2, §15) because the usual Rust
binding links `libzmq`, a C library, and the engine is one static binary.
The [`zeromq`](https://crates.io/crates/zeromq) crate (0.6, MIT, maintained
under the zeromq GitHub organisation) implements ZMTP in Rust on tokio: no C,
no build script, no dynamic library. With only the TCP and IPC transports it
adds nine crates to a Linux build (`zeromq`, `futures`, `asynchronous-codec`,
`crossbeam-queue`, `regex`, `scc`/`saa`/`sdd`, `tokio-util`), and six more
to the lock file that only Windows builds use (IPC there: `win_uds` and its
`async-io`). That is still more than nothing, so a plain build leaves it out;
the Docker image builds it in (`ARG ENGINE_FEATURES=zmq`;
`--build-arg ENGINE_FEATURES=` leaves it out). Built in, it does nothing
until a node setting names a `zmq_pub`: no socket, no task.

## How it works

It is a facade over polling, not a replacement:

```
monerod --zmq-pub ──► node_events::run_subscriber ──► NodeWakes ──► loops
                       (one per network, under           pool  ──► fast mempool pass
                        its stop signal)                 chain ──► scheduler round
```

- `node_events::NodeWakes` (always built) holds two `tokio::sync::Notify`s
  on each network's `ScanState`. The fast mempool loop waits on `pool`, the
  round loop on `chain`, each for at most its usual interval. A wake-up is
  stored if nobody is waiting, so an announcement during a pass makes the
  next one start at once; many announcements make one wake-up. Passes are
  never closer than 20 ms, so a burst of transactions is taken in a few.
- `node_events::run_subscriber` (`zmq` feature) subscribes to
  `json-minimal-txpool_add` and `json-minimal-chain_main` on each
  configured `zmq_pub` (the node and its fallbacks) and pokes the wakes.
  Only the topic is read, never the JSON. It reconnects after 1 s doubling
  to 60 s, logs a throttled warning while it can't, and wakes both loops
  whenever it (re)connects, for anything missed meanwhile. Saving node
  settings restarts it with the new list. The `zeromq` socket would
  reconnect by itself, silently; the subscriber watches the socket's events
  instead, ends the connection when the node goes, and reconnects itself,
  so a lost connection shows on `/status` within moments.
- A pool wake-up also calls `MoneroDaemonClient::pool_changed`, so the
  pass asks the node instead of reusing the answer `RpcDaemonClient` keeps
  for 100 ms (`POOL_REUSE`), which would no longer describe the pool.

What is scanned and recorded still comes only from RPC answers, from the
node the pass pins, through the same code as without the feature. An
announcement only says "ask now". So:

- a lost announcement (monerod drops on a full queue; ZMQ PUB/SUB has no
  delivery guarantee) costs at most the poll interval it would have cut;
- a late, duplicated or forged one costs one early, small request;
- an announcement from a fallback while another node is pinned is harmless;
- reorgs, removed pool transactions and everything else polling handles
  are handled exactly as before.

## What isn't announced (Dandelion++)

From monerod's source (`master`, October 2026):

- `core::add_new_tx` publishes `txpool_add` only when the transaction's
  relay method `matches_category(relay_category::legacy)`
  (`cryptonote_core.cpp`). In `blockchain_db.cpp` that is `fluff`,
  `block` and `none` (submitted by RPC with `do_not_relay`). `stem` (a
  Dandelion++ stem hop through this node), `local` (submitted to this node
  by RPC, waiting to go out over Tor or I2P) and `forward` (received over
  Tor or I2P, on its embargo timer) are not announced.
- Nothing announces such a transaction later: when this node fluffs it,
  or its embargo ends, `tx_pool.cpp` changes its relay method but publishes
  nothing.
- RPC hides the same transactions until they are public: without
  `include_sensitive`, the pool is read as `relay_category::broadcasted`.

So a payment whose transaction reaches our node in its stem phase, or over
Tor or I2P, is never announced; the poll finds it once the node makes it
public. How long that takes after the transaction goes public is the poll
interval, which is why polling stays on and why slowing it down (below)
needs care. Most transactions reach a node already fluffed (a stem is a
few hops long and our node is on few of them), so most are announced.

## Configuring it

On the node (its own machine or a private network: ZMQ has no
authentication or encryption here):

```
monerod --zmq-pub tcp://127.0.0.1:18083
```

`--no-zmq` turns the publisher off too (monerod warns and ignores
`--zmq-pub`).

On the engine (any build: `zmq` is a default feature), add `zmq_pub` to the
node setting
(`monero_node.<network>`, a fallback can have its own), or fill in
"Announcements (ZMQ)" on the node's row of the admin nodes form:

```json
{"host":"127.0.0.1","port":18081,"zmq_pub":"tcp://127.0.0.1:18083",
 "fallbacks":[{"host":"node.example","port":18089}]}
```

`zmq_pub` is `tcp://host:port` or `ipc:///path`, checked when saved. An
engine built without the feature refuses a node setting that has one
(the save fails; at boot, an invalid setting stops the process), so it is
never accepted and silently ignored.

## What it gains

| | Polling only (defaults) | With announcements |
|---|---|---|
| Pool transaction to scanned | up to 250 ms, plus a round trip | a round trip (≥ 20 ms after the last pass) |
| Block to scanned | up to 1 s, plus the round | the round |
| Requests while idle | unchanged | unchanged |

At the defaults the saving is modest: about 125 ms on average for a 0-conf
payment and 500 ms for a block. Everything after detection (the database
commit, the engine's order event stream, monokulo's `LiveHub`, the
checkout's SSE) is already push, so detection is the only wait left.

The larger gain is the next step: with announcements arriving, polling only
has to catch what they miss, so it can be slowed down a lot.

## Seeing it

The engine's `/status` has, for each network with a publisher configured,
`announcements`: each publisher (its node, endpoint, whether connected and
since when, connections made, pool and block announcements counted, the
last one's time, the last failure and when) and how many pool passes and
scan rounds an announcement started early. monokulo's status page shows
this to admins only, as an "Announcements (ZMQ)" table under each network;
anyone else sees nothing of it, as publisher addresses and errors can name
internal hosts.

## Not done yet

1. **Slower polling while announcements arrive.** Today every poll still
   runs on its own timer (250 ms for the pool, 1 s for a round); with
   announcements, nearly all of them answer "nothing new". While a
   publisher is connected the polls only have to catch what isn't
   announced (the Dandelion++ and Tor cases above, a dropped message,
   transactions leaving the pool), so they could run every few seconds:
   four to ten times fewer requests, with announced payments seen as fast.
   The cost is the unannounced ones: a payment that went through our
   node's stem would wait up to the slower interval after going public.
   Something like 2 s for the pool is a fair trade; it should be a setting.
   "Connected" now comes from the socket's own events, so the switch back
   to full speed on a lost connection is immediate.
2. **A test against a real monerod** (`--regtest --zmq-pub`, as in
   `docs/TESTING.md` §10): a payment broadcast and a block mined, each seen
   well inside the poll interval.
3. **The released binaries.** CI's release tarballs are built without the
   feature; only the Docker image has it.
4. **Tor and remote nodes.** The subscriber dials directly; it is meant for
   one's own node. A node reached over Tor stays polling-only.

## Code

- `crates/engine/src/node_events.rs`: `NodeWakes`, the subscriber, tests
  (including one against a real `zeromq` publisher).
- `crates/engine/src/loops.rs`: the loops wait on `NodeWakes`; the
  subscriber starts beside them.
- `crates/engine/src/settings.rs`: `MoneroNodeSetting::zmq_pub` and its check.
- `crates/engine/src/daemon.rs`, `daemon_rpc.rs`, `daemon_fallback.rs`:
  `pool_changed`.
- `crates/monokulo/src/admin_nodes.rs`, `views/admin.rs`: an optional
  "Announcements (ZMQ)" box on each node row of the admin nodes form, so
  saving the form keeps a node's `zmq_pub` instead of dropping it.
- `crates/shared/src/announcements.rs`: what `/status` reports;
  `crates/engine/src/http/status_page.rs` fills it in,
  `crates/monokulo/src/http/status_page.rs` and `views/status.rs` show it.

CI runs the tests with the feature (as the Docker image is built), and
the one test that only exists without it (`test(/without_zmq/)`).

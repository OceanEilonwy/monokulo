# Announcements from the node (ZMQ)

Status: prototype, behind the engine's `zmq` cargo feature (off by default).

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
`async-io`). That is still more than nothing, so it stays opt-in.

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
  settings restarts it with the new list.
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

## Configuring it

On the node (its own machine or a private network: ZMQ has no
authentication or encryption here):

```
monerod --zmq-pub tcp://127.0.0.1:18083
```

`--no-zmq` turns the publisher off too (monerod warns and ignores
`--zmq-pub`).

On the engine, built with `cargo build -p engine --release --features zmq`,
add `zmq_pub` to the node setting (`monero_node.<network>`, a fallback can
have its own):

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

## Not done yet

1. **Slower polling while announcements arrive.** While a subscriber is
   connected, stretch the fast pool pass (250 ms) and the round (1 s) to,
   say, 5 s and 10 s: four to ten times fewer requests against one's own
   node with no loss of latency. Needs the subscriber to report "connected"
   (the zeromq socket monitor, or "a message in the last N minutes"; a
   quiet stagenet can go many minutes between transactions, so silence alone
   doesn't prove a dead link).
2. **`/status`.** Show per node whether its announcements are connected and
   when the last one came, beside the RPC counts.
3. **A test against a real monerod** (`--regtest --zmq-pub`, as in
   `docs/TESTING.md` §10): a payment broadcast and a block mined, each seen
   well inside the poll interval.
4. **Dandelion++.** Check whether monerod announces a transaction while it
   is in its stem phase, or only once it fluffs. Either way the RPC poll is
   what records it; this only changes how early the wake-up comes.
5. **Packaging.** Decide whether release builds (the Dockerfile, the musl
   targets) turn the feature on. Building it in costs the nine crates
   above; a merchant who doesn't set `zmq_pub` gets no socket and no task.
6. **Tor and remote nodes.** The subscriber dials directly; it is meant for
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

Tests run both ways: `cargo test -p engine` and
`cargo test -p engine --features zmq`.

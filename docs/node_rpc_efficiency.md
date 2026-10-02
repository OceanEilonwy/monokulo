# What the engine asks of a Monero node

Status: implemented. This is the record of what is asked, why, and what it
costs.

Only the engine (`crates/engine`) talks to `monerod`. Monokulo never does:
it reads the engine's `/status` (cached for 10 seconds) and its admin API.
Every request goes through `RpcDaemonClient` (`src/daemon_rpc.rs`), one per
configured node, behind `FallbackDaemonClient` (failover, cooldowns, one node
pinned per round).

A node is often someone else's public node, reached over Tor or a metered
line. So the rule is: ask for nothing that isn't used, ask for it in the
smallest form the node offers, and ask once.

## What is asked, and when

| When | Request | Size |
|---|---|---|
| Every round (each second, by default) with nothing to watch in the pool | `/get_height`: the tip's height and id, in one answer | about 150 B |
| Every round with something to watch in the pool (a store with an order in scope, or an unconfirmed payment) | one `get_blocks.bin`: what entered and left the pool since the last answer, and whether the chain still ends at the tip last seen | about 150 B when nothing changed; about 0.4 kB per new transaction |
| Such a round when the tip has moved (a new block, every two minutes or so) | that, plus `/get_height` for the new tip | about 0.6 kB more |
| Every round, while the recorded chain ends below the node's tip | `on_get_block_hash` for the highest recorded block | 119 B |
| Every fast pass (4 a second), while a store has an order in scope | `get_blocks.bin`, pool only: what entered and left the pool since the last answer | about 150 B when nothing changed; about 0.4 kB per new transaction |
| Every minute, while the pool is being followed | `/get_transaction_pool_hashes`: the plain list, to correct a missed change | about 70 B per pool transaction |
| A new block, for a store with an order in scope | `get_blocks.bin` with pruned transactions | about 13 kB a block on mainnet |
| A new block nobody is scanned for | `get_block_headers_range` | about 1 kB a block |
| A page of unconfirmed payments that left the pool | one `/get_transactions` (pruned) for all of them, then one `/is_key_image_spent` per node for those found nowhere | about 1 kB per transaction |
| A reorg | `on_get_block_hash`, O(log depth) times; one `/get_transactions` per page of affected payments | |
| A page of recently voided payments, every five minutes | one `/is_key_image_spent` per node | |
| With more than one node: a new recorded block, every 30 s, or every round while settlement is held or the agreed block is below the recorded top (`docs/chain_agreement.md`) | `/get_height` and `on_get_block_hash` per node, all nodes at once; more hash lookups only when the top block isn't agreed (at most 6) | about 270 B a node |
| `/status` | `get_info` once per node, all nodes at once | about 1.4 kB a node |
| Payment lookup by txid (admin) | one `/get_transactions` (pruned), then `/get_height` if it pays an order | |

While no store has an order in scope and no payment is unconfirmed, a round
is the first row alone: one request of about 150 bytes. The pool is not
polled and the fast mempool loop asks nothing. With something to watch and
no new block, a round is the second row alone: still one request.

## How each is kept small

**Pruned transactions.** A scan reads a transaction's prefix (output keys,
key images, the transaction public key) and its RingCT base (the encrypted
amounts). The ring signatures and range proofs, five sixths of the bytes, are
never read. `get_blocks.bin` and `/get_transactions` are asked with
`prune: true`, and `shared::monero_tx` decodes what comes back (`monero` 0.22
decodes only whole transactions).

A pruned transaction doesn't hash to its id, so the id travels with it
(`ChainBlock::txid`, `FetchedTx`). It is still checked: the node sends the
hash of the pruned-away part, and the id is the hash of (prefix hash, base
hash, that hash). In a block, the id computed that way must be the one the
block itself lists for that position; from `/get_transactions`, the one the
node named. A mismatch fails the call. Only where nothing can be computed (a
version 1 transaction, or a body that arrived with the pool's changes, which
carry no such hash) is the id taken as given.

Key custody is given a scan input made from each transaction
(`ScanInput::of`: the prefix and the RingCT base), which a pruned
transaction has in full. A payment is recorded under the id its
transaction came with.

**A block's identity from the block.** `get_blocks.bin` returns each block's
blob with its transactions. The blob's hash is the block's id, its header
carries the parent's id and the time, and its coinbase carries the height.
So nothing else is asked about a block, and a node answering from the wrong
height is refused instead of scanned. (The genesis block, which
`get_blocks.bin` can't be asked for by height, is read from its header: it
holds no transactions but its coinbase.)

**Reorg detection without a lookup.** Hashes chain, so the recorded chain
matches the node's if the highest recorded block does. `/get_height` returns
the tip's id along with its height; while the recorded chain ends at the tip
that comparison costs nothing. Only while the node is ahead of what is
recorded (a block just arrived, or a catch-up) is one hash looked up, with
`on_get_block_hash` (119 bytes) rather than `get_block` (2.4 to 15 kB).

**The pool by its changes.** `get_blocks.bin` with `requested_info` = "pool
only" and `pool_info_since` = the node's clock at its last answer returns
what entered the pool (with pruned bodies) and what left it: the request
wallets poll with. The client keeps the set of ids between polls
(`PoolView`), hands the bodies over when the scanner asks for them, and
reuses an answer for 100 ms so the round and the fast loop don't both ask.
Once a minute the plain list of ids replaces what was followed, so a change
missed across a node restart or between a load balancer's backends lasts at
most that long.

A node that doesn't describe its pool in answer to that request (an older
monerod) is asked for the plain list each time, as before, and tried
again after ten minutes.

**The tip with the pool.** A round that looks at the pool needs the tip too.
`get_blocks.bin` can be asked for blocks and the pool at once
(`requested_info` = "blocks and pool"), and a wallet's request for blocks
names the block it has (`block_ids`). monerod answers a request that names
its own top block with no blocks at all: just the chain's length, and the
pool's changes. So the round's poll names the tip the node last gave
(`MoneroDaemonClient::get_tip_and_mempool`), and "no blocks" means that tip
stands: its height and id are already known, and `/get_height` isn't asked.
When the tip has moved the node sends a block instead. One is asked for,
from the start of the chain (`start_height` 1, `max_block_count` 1: a few
hundred bytes, of no interest in itself), and the new tip is read from
`/get_height` as usual. That happens once a block.

The two answers stay two answers: a pool that can't be read doesn't hide
the tip, and the other way round. A node that doesn't answer the two
together as monerod does (no description of the pool, a run of blocks where
one was asked for, a length that doesn't fit the tip named) is asked them
apart for ten minutes, then tried again. The once-a-minute correction by the
plain list is a round of two requests too.

**Nothing fetched that nobody reads.** Pool bodies are fetched only when
there is a store to scan them for. A new block with nobody to scan it for
(no store with an order in scope, or none with its keys registered) is
recorded from its header; stores that were left behind catch up on whole
blocks later.

**Many payments, one request.** `locate_transactions` asks where a page of
transactions is in one `/get_transactions`. It is a hint: only an affirmative
answer (a miss the node names, or an entry that places the transaction) is
used, and anything else is asked about singly, where a non-answer is an
error and never "not found". Key images of several payments go in one
corroborated `/is_key_image_spent` per node, and the nodes are asked at once.

A payment whose transaction is nowhere and isn't proven double-spent
(dropped or evicted) is looked at again at once, twice, then at doubling
intervals up to a minute, instead of every round for as long as it stays
that way.

## Measured

Against public mainnet nodes (September 2026):

| | Before | After |
|---|---|---|
| A block hash | `get_block`: 2.4 to 14.8 kB | `on_get_block_hash`: 119 B |
| 20 blocks, 537 transactions | 1,118 kB | 260 kB |
| 61 transactions by id | 633 kB | 77 kB |
| The pool, 66 transactions, polled | 4.8 kB every poll | 28.6 kB once (with bodies), then 0.15 to 2 kB a poll |
| 21 headers | 21 × `get_block`: 193 kB | 18.8 kB |

## Seeing it

`RpcDaemonClient` counts every request and its bytes by endpoint. The
engine's `/status` lists them for each node under `rpc` (busiest first), so
what a deployment costs its node can be read off rather than estimated. Pool
polls are counted apart from block fetches, as
`/get_blocks.bin (pool changes)` and, when they ask about the tip too,
`/get_blocks.bin (pool changes and tip)`.

## Tests

- `tests/daemon_rpc_replay.rs` replays a recording of a real stagenet node:
  every request above, pruned and whole forms compared transaction for
  transaction. Re-record it when a request changes
  (`cargo test -p engine --test daemon_rpc_replay -- --ignored`).
- `daemon_rpc::wire_tests` script a node: the pool followed by its changes,
  the fallback for a node that can't say them, the tip asked about with the
  pool (unmoved, moved, and a node that can't answer both), batched lookups,
  id checks.
- `work::tests` hold the scheduler to a budget with a fake node that records
  every call: one request a round while idle, one call for the tip and the
  pool while watching, headers for blocks nobody is scanned for, two round
  trips for a page of vanished payments.
- The ignored live tests in `daemon_rpc::live_node_tests` run the same
  requests against a mainnet node (`ENGINE_LIVE_TEST_NODE=host:port` names
  another than the default).

## Not done

- ZMQ push from one's own node: behind the `zmq` feature (on in the Docker image)
  (pure Rust, no `libzmq`) as a wake-up over polling
  (`docs/monero_zmq.md`). Slowing polling down while it is connected,
  which is where the request savings would come from, is not done.
- Compression: monerod doesn't offer it. A proxy in front of a node might;
  it isn't asked for.

## Removed

`MoneroDaemonClient` holds what the engine asks of a node and nothing else.
Calls nothing in the engine made any more were taken out of it and its
implementations, with the tests that only tested them:

- `get_mempool_transactions` (`/get_transaction_pool`, every pool
  transaction whole): the pool is read by its ids and changes.
- `get_blocks_range` (`get_blocks.bin` with whole transactions, no block
  ids): blocks are read with `get_chain_blocks`.
- `find_height_at_or_before` (a binary search over block timestamps, for
  the manual rescan that was removed earlier).
- `get_block_transactions`, `get_block_timestamp`, `get_transaction` and
  `get_transactions` (`get_block` and whole `/get_transactions`): the
  engine reached them only through trait defaults that the real client
  overrode. `get_chain_blocks` and `get_transactions_with_ids` are what a
  client implements now, and the test doubles with it. With them went the
  real client's whole-transaction decoding, and the scanner's
  hash-a-whole-transaction entry points (`scan_transaction`,
  `scan_transaction_for_tenant`, `tx_id_hex`), which only tests called:
  they are test helpers now.

The client never asks for a whole transaction. Where a test wants one to
hold a pruned one against (the replay test, the live tests), it makes that
request itself.

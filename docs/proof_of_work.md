# Checking proof of work

Status: implemented.

## The question

The engine counts a payment's confirmations on blocks its node serves. A
node can make blocks up: an attacker builds a real transaction to an
order's address, shows it only to a node they control (so they keep the
coins), and serves it in a made-up block with as many made-up blocks on
top as the store asks for. The order is paid, the webhook fires, the shop
ships.

Checking each block's proof of work turns that free lie into real mining at
the network's difficulty. Is it light enough to do in the engine?

## Is it worth doing?

Yes. Measured on a Ryzen 9 5950X with RandomX's own library
(`randomx-rs`, as Cuprate uses), in light mode:

| | |
|---|---|
| A block's hash (JIT) | about 16 ms on one core |
| The same, interpreted (where the JIT can't run) | about 140 ms |
| Building a RandomX key's cache (each 2048 blocks, and at start) | about 0.25 s |
| Memory, while checking is on | 256 MiB per network for the key's cache; never more than two keys (512 MiB), held for a moment around a key change |
| Steady state | 720 blocks a day: about 12 s of CPU a day |
| Taking an anchor | 64 sampled hashes, then the 720 blocks up to the tip: about 13 s of CPU and 7 MB, over three rounds |
| Catching up after downtime | about 16 ms a block: a day is 12 s, a week 81 s |
| Fetching | `get_block` per block, about 9 kB on mainnet (6 to 9 MB a day) |
| Delay added to a settlement | up to the proof loop's poll (5 s), then the fetch and 16 ms; with ZMQ announcements, just the fetch and the hash |

A test against 30 recent mainnet blocks recomputed every difficulty exactly
as the node reported it, found every proof valid, and refused every block
with one bit of its nonce changed.

What it buys: with one node, making up the N blocks an order needs costs N
blocks of real mining at the network's difficulty (about N × 0.6 XMR of
mining). With more than one node, the engine follows the heaviest valid
chain any of them serves, so an attacker must out-mine the whole network,
and one honest node among liars is enough. It doesn't stop someone with
that much hash power, nor a majority miner reorganising the real chain
(the 18-block reorg of September 2025): confirmation counts are the defence
there.

## Turning it on and off

`proof_of_work.mainnet`, `proof_of_work.stagenet`, `proof_of_work.testnet`:
a checkbox per network on the admin page's Nodes tab, in that network's
block. On for mainnet by default, off for the test networks (their coins
are worth nothing). Turn it off for a node you fully trust, such as your
own on a router with little memory.

When the engine starts with checking on, it is turned on before any scan
loop runs. Ticked while running, it takes hold at the proof loop's next
round (within seconds). From then on, settlement on that network is held
until an anchor is taken and blocks are proven. Turning it off frees the
memory and forgets the anchor and proven chain; turned on again, it starts
afresh.

## Design

### The rules (`pow`)

Pure functions, as monerod applies them (`cryptonote_config.h`,
`difficulty.cpp`, `hardforks.cpp`):

- `seed_height`: the block whose id is the RandomX key, changing 64 blocks
  after each 2048th.
- `next_difficulty`: from the 735 blocks before, the newest 15 left out,
  timestamps sorted and 60 cut from each end, cumulative difficulties read
  at the same positions unsorted. 128 bits.
- `check_hash`: the hash as a little-endian 256-bit number, times the
  difficulty, must fit in 256 bits.
- Timestamps: not below the median of the last 60, not more than two hours
  ahead of this machine's clock.
- `decode`: the block's id and RandomX input are computed from its blob,
  never taken from the node; the coinbase must name the height asked for.

A rejection says what it means for the node: `Invalid` (it served a block
breaking a rule), `NotYet` (a timestamp ahead of the clock: asked again
later, nothing held against it) or `Moved` (its chain changed under us).

`pow::hasher` runs RandomX on a thread of its own per network: its objects
can't leave the thread that made them, and hashing mustn't block Tokio. The
JIT runs with `FLAG_SECURE` (pages never writable and executable at once:
the programs it compiles come from blocks a node sent); if it can't be set
up, hashing is interpreted, with a warning. A key unused for a minute is
dropped when another is used.

### The anchor (`proof::anchor`)

One block, with the 735 before it, taken on the nodes' word,
`anchor_depth` (720) blocks below their tips: far below any reorg Monero
has seen.

A block's id commits to its timestamp and parent but not its difficulty,
which is computed from every block back to genesis. So the window's claimed
difficulties are the one thing trusted, hedged three ways:

1. A majority of the *configured* nodes (one of one, two of two, two of
   three) must give the same window, row for row, and the same RandomX key
   below it. The height is the highest at least that many nodes reach, so a
   node claiming to be far ahead can't choose it.
2. Every claimed difficulty must be at least the network's floor
   (mainnet 100 G, against about 750 G now; stagenet 100 k; testnet 100),
   the cumulative ones must add up, and the difficulty the window gives the
   block after the anchor must be the one the nodes claim for it. Its dates
   must be about where the clock says: the anchor between a quarter and four
   times 720 blocks' time old, and the window spanning between a quarter and
   four times its blocks' time. A window dated far back would otherwise let
   the blocks after it bend the difficulty down.
3. 64 of the window's blocks, at random and always the anchor, must have a
   proof of work meeting the difficulty claimed for it. A made-up window
   costs real mining at least at the floor, block for block.

With one node, its word is all there is: the floor, the dates and the
samples are what stand between it and a made-up anchor. Until an anchor is
taken, the status says why (too few nodes answered, no majority, a rule
broken) and nothing on the network settles. A failed attempt isn't retried
for a minute, doubling to ten: a fresh random sample every round would let
a window with a few bad blocks through by retries.

### Following (`proof::Follower`)

Each round, every configured node is asked for its tip:

- A tip on the proven chain (at or below its tip, the same block there):
  **on chain**.
- A chain going past the proven tip, or leaving it lower down: where it
  leaves is found by binary search with `on_get_block_hash`, and its blocks
  from there are fetched (8 at a time) and checked, at most 64 a batch so
  every block's RandomX key is known before the batch. Within a batch,
  blocks the proven chain already has are passed over unhashed (the node's
  word on where its chain leaves ours costs it nothing to bend); the rest
  have every rule but the hash checked in order, then are hashed, then their
  time is checked against the clock, so a bad proof is caught whatever its
  time. Every block's difficulty must also be at least the network's floor:
  however its timestamps were bent, a made-up chain costs that much work a
  block. As soon as the branch has more cumulative difficulty than the
  proven tip, it replaces the proven chain above where it left (an
  extension of the tip does at its first block), committed batch by batch.
  A branch with as much work as the proven chain doesn't replace it: the
  first seen stays, as in monerod.
- A whole branch checked and still lighter: **lighter**. Its tip isn't
  checked again until it changes.
- A chain leaving the proven one below the deepest block with a whole
  window kept: **diverged**, can't be followed.
- A tip below everything kept (syncing, or long down): **unknown**, not
  held against it.
- A block breaking a rule, or answers that contradict each other (a tip
  that isn't the proven block there, then the proven chain all the way up
  to it): **caught**. Its tip isn't checked again until it changes.

The node claiming the highest tip is looked at first. Each node gets its
own 256 blocks a round (a branch below the proven tip gets what it needs to
outweigh it), so one node can't use up another's; a catch-up carries on in
the next round a tenth of a second later.

A caught, lighter or diverged node is excluded from scanning
(`FallbackDaemonClient::set_excluded`, never every node): never pinned,
never asked in `locate_transaction_corroborated` or
`is_key_image_spent_corroborated`. The scanner then pins a node on the
proven chain, its reorg detection finds where the recorded chain left it,
and the payments found on the other chain are reconciled away. A node is
let back once its tip is on the proven chain again; a caught one not before
an hour has passed, so it can't get back in by answering honestly for a
round. If excluding them would leave no node, only the caught ones are left
out; if they are every node, none is.

The proven chain keeps 30 days of block ids (at least the 735 blocks below
`tip - anchor_depth`), and the RandomX keys still needed beside it
(`proof_seeds`). So a reorg up to 720 blocks deep is followed; a deeper one leaves every node
**diverged** and the network **held**: the status says so, and an operator
who has checked the nodes takes a new anchor (the status page's button,
`DELETE /api/v1/admin/proof/{network}/anchor`). That is never automatic: one
lying node could otherwise force it.

### Settlement

A payment's height alone proves nothing: it can come from a node's word
(`locate_transaction`, after a reorg or a payment lookup), and a recorded
block can be one a lying node served before it was caught, kept below the
scanner's reorg window. So each payment carries the id of the block it was
found in (`order_payments.block_hash`): a scan stamps it with the block it
scanned (its id computed from the block); a height taken from a node is
stamped only once that block's own list of transactions, from `get_block`,
holds the transaction; any new height clears it.

`Store::proven_views` gives an order's payments as proven: one counts only
if its block is the proven block at its height, and only with the
confirmations up to `Store::proof_ceiling` (the highest block both recorded
by the scan and proven, by id); one that isn't counts none. `plan_status`
derives the status from the real confirmations and from these; a
settlement they don't reach waits, the order shows `confirming` with its
real counts, and is looked at again next round. 0-conf acceptance has no
block and is unaffected.

### Restarts

The anchor, the proven chain and the keys are in the database. After a
restart the first round rebuilds the key's cache (a quarter of a second)
and carries on from the proven tip; a crash mid-batch repeats that batch.
The exclusions are in memory and found again in the first round.

### Seeing it

`/status` gives each network's `proof` (absent while off): the state
(`anchoring`, `following`, `held`), a sentence, the anchor and how many
nodes agreed, the proven tip, the ceiling, each node's verdict, height and
whether it is excluded, blocks checked, and the hashing (JIT or not, mean
times, keys held). Monokulo shows it to admins only, in the network's card
on the status page (`views::status`):

- the state as a tag beside the network's name ("proof checked", "finding
  an anchor", "settlement held");
- each node's verdict in the Nodes table's Proof of work column, what it
  was caught on under it, and "left out" in its Use column;
- while following, one sentence ("Orders settle on blocks up to …, proven
  by the engine itself.") with a chip saying how far behind the tip that
  is, over a window of the last 30 blocks at a fixed scale: proven blocks
  filled, ones only seen outlined, the block orders settle up to ringed
  and the tip marked. The tip is the highest a node on the proven chain or
  past it reports, never a caught node's claim. The chip is the same
  neutral tag at every count;
- while held, the engine's reason and the "Take a new anchor" button in
  one box, in place of the window;
- the anchor, blocks checked, hashing, keys and last check folded under
  "How it's checked".

## Edge cases

- **The difficulty isn't in the block**: the anchor's window is agreed,
  floored and sampled; every later difficulty is computed here.
- **A node far ahead, real or not**: it can't choose the anchor; its blocks
  are checked from where it leaves the proven chain; a made-up one is
  caught at its first bad block.
- **Nodes behind or syncing**: on chain (behind), or unknown below what is
  kept; the anchor waits for a majority to reach a height.
- **A payment's height from a node's word**, or a payment in a block
  recorded from a node caught later: it settles only once found in the
  proven block at its height.
- **A liar's cost to the engine**: its bent answers are cheap to check
  (blocks we have pass unhashed, a caught or lighter tip isn't checked
  again until it changes), every node has its own budget, the RandomX
  keys are capped at two, and anchoring backs off.
- **A key change** every 2048 blocks: two caches for a moment; blocks are
  batched so their keys are known; a reorg replacing a key block changes
  the key, read from the branch being checked.
- **Timestamps ahead of the clock**: not yet, not invalid. An engine clock
  more than two hours slow defers honest blocks: keep it in sync.
- **A hard fork** that keeps RandomX passes; one that changes the proof of
  work makes honest blocks fail, and settlement holds (safely) until the
  engine is updated. Blocks before RandomX are never checked: an anchor's
  window must start after it.
- **Two nodes disagreeing about an anchor**: neither is a majority of two;
  nothing settles until they agree.
- **Every node off the proven chain**: none is excluded (`set_excluded`
  refuses), the network is held.
- **The scanner and the proof loop on different nodes or branches**: the
  ceiling is where their records meet, and each payment counts only in its
  proven block.
- **Turning it off and on**: off forgets everything; on starts afresh, with
  settlement held from the first moment.
- **Node settings saved**: a new fallback client; the next round excludes
  again.

## Testing

1. **The rules** (`pow::tests`): 10 real mainnet blocks across a key change
   (`pow/fixtures`), difficulty and proof exactly as monerod; nonce,
   timestamp, transaction list and key each changed fails; RandomX's own
   reference vector; each rule's edges (median of an even count, cuts and
   lag, values past 64 bits, `check_hash` at exactly 2^256).
2. **Storage** (`store::proof::tests`): anchoring, extending, switching,
   a stale parent refused, pruning keeps keys, the ceiling follows the
   recorded chain and its rewinds, turning off forgets; a payment counts
   only once attested in its proven block, and a new height forgets that.
3. **Settlement** (`store::tests`): deferral under a ceiling, counts kept,
   0-conf unaffected.
4. **Following** (`proof::tests`), against fake nodes serving real blocks
   mined for real at difficulty 2 (`pow::test_chain`): an honest chain
   across a key change; a made-up block caught and the node back once
   honest; a lying primary of three excluded; a heavier branch replacing
   the proven chain, a lighter or equal one not; a reorg deeper than can be
   followed held, then a new anchor; anchor majorities, the floor, a failing
   sample and a lying next difficulty; a restart; a catch-up over rounds;
   future and stale timestamps; pruning; turning off; an unreachable node;
   a caught node kept out for its hour; a node bending block hashes costing
   one hash; a node contradicting itself caught; a node far behind not held
   against it; a block below the difficulty floor caught; a window dated
   far back refused; a failed anchor not retried at once.
5. **End to end with the scanner**: an order paid only once its
   confirmations are proven; never on made-up blocks; a lying primary of
   three excluded, its payment reorganised away, the honest chain scanned;
   a payment 40 blocks deep in a made-up chain (past the scanner's reorg
   window) never paid.
6. **Admin and RPC**: `DELETE /api/v1/admin/proof/{network}/anchor`;
   monokulo's button route (admins only, the engine asked, refusals);
   monokulo's section shown to admins only, the button only while held; the
   checkbox saved from the Nodes tab, inside its network's block;
   `get_block` and `get_block_headers_range` parsed as monerod sends them,
   128-bit difficulties included. RandomX holds at most two keys.

## Not covered

- **0-conf**: a pool transaction is only ever the node's word.
- **Transaction validity**: a block with a valid proof of work is taken to
  hold valid transactions. Faking one still costs the mining.
- **A majority miner**: the real chain itself; confirmation counts are the
  defence.
- **Payments older than 30 days, never settled**: their block is no
  longer among the proven ids, so they don't settle by themselves under
  checking.
- **Scanned blocks fetched twice**: the scan's `get_blocks.bin` already
  carries each block's blob; handing it to the proof loop would save the
  9 kB `get_block` per block.

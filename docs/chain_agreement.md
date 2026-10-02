# Chain agreement across nodes

Status: implemented.

## The question

Should the engine sometimes check that most of its configured nodes agree
with the chain it is following, so that one hijacked node can't lead it
astray? Only when more than one node is configured, and allowing for honest
nodes briefly showing different tips.

## Is it worth doing?

Yes. Here is why.

**What one bad node can do today.** Each round pins one node (the first out
of cooldown) and believes it. The engine checks hashes chain together
(`work::chain::detect`), but it doesn't check proof of work: verifying
RandomX in the engine is out of scope. So the pinned node can:

1. **Make up blocks.** It can serve blocks that pay an order, with as many
   made-up blocks on top as the store requires. The payment itself is
   real-looking: the attacker builds a genuine transaction to the order's
   address from their own coins, but only ever shows it to the lying node,
   so it never reaches the real chain and they keep the coins. The order
   becomes `paid`, its webhook fires, and the shop ships. This is the
   expensive one.
2. **Hide or delay.** It can serve a stale chain, or leave payments out.
   Orders wait or expire. This is bad, but nothing false is recorded.
3. **Fake the pool.** It can show made-up 0-conf transactions. That is
   trusting the node by nature; see "Not covered".

Reorg detection can't catch (1), because it compares our recorded chain
with the *same* node that produced it. A fallback is only consulted when
the primary fails, and a lying node doesn't fail.

**Why "majority tip" is the wrong measure.** Tips differ all the time
between honest nodes: one is a block behind, or two have competing blocks
at the same height for a minute. A tip is also only the node's claim.
What matters is narrower: *are the blocks we recorded, the ones payments
were found in, on the chain most of our nodes have?* Asking each node for
the hash at a height we recorded (`on_get_block_hash`, 119 bytes) answers
that directly. It is anchored to our own record, so no node can choose
what is compared.

## Design

### Votes and verdicts (pure)

For one recorded block (height `h`, hash `x`), each configured node votes:

- `Agree` if it has `x` at `h`;
- `Disagree(y)` if it has another block `y` at `h`;
- `Abstain` if it doesn't answer, or doesn't have `h` yet (behind).

`verdict(votes)` at one height (`a` = agrees, `d` = disagrees):

| Verdict | When |
|---|---|
| `Agreed` | `a >= 2` and `a > d` |
| `Outvoted` | the largest group of disagreeing nodes on one hash has at least 2 nodes and more than `a` |
| `Contested` | a disagreement that neither side wins |
| `Unverified` | fewer than 2 opinions (`a + d < 2`) |

"At least 2" means at least one node other than the one that gave us the
block. A node agreeing with itself proves nothing.

### One check: step down from where two nodes can speak

A check first asks every node for its height (`/get_height`, about 150
bytes). It starts at `start_height`: the lower of our recorded top and the
*second-highest* node height, the highest block at least two nodes can
speak to. Without this, a node serving a made-up chain *ahead* of the real
one would only ever meet silence from the others ("they don't have that
height yet"), which reads as unverified and would eventually lift the
ceiling. With it, that chain is compared where the honest nodes are, and
they disagree with it.

From there the check steps down, at most 6 heights (`WINDOW`), and stops at
the first height with `Agreed`. Then:

- **Agreed at `h`.** Every recorded block up to `h` is on the majority's
  chain. A fresh honest fork, or a node a block behind, costs a step, not
  an alarm. The outcome names the nodes *off* the majority's chain (another
  block at `h`, or outvoted on one of ours above `h`), each with the lowest
  height it was caught at. It also names the nodes shown to be *on* it.
- **No agreement in the window.** The outcome is the verdict at the
  deepest height tried. `Outvoted` there means our chain left the
  majority's at least that far back, and the nodes that voted for our
  chain are named.
- Otherwise the outcome is `Contested`, or `Unverified` if no height got
  two opinions. Nothing recorded yet is also `Unverified`, so the first
  round, which records the first blocks, can't settle on them unchecked.

All nodes are asked about a height at once. A check costs about
`nodes × (150 B + steps × 119 B)`, usually `nodes × 270 B`.

### The state machine

Each network keeps one `AgreementState`, changed only by
`AgreementState::next(input, now)`. That function is pure, matches every
(state, input) pair exhaustively, and returns the new state with its
`Effects`:

```
            ┌──────────── node list shrinks to one ───────────┐
            ▼                                                  │
 Single ─(2+ nodes)─► Checking ─(Agreed h)─► Agreed{h} ◄──────┤
                         │                     │  ▲            │
                         │ Contested/Outvoted/ │  │ Agreed h   │
                         │ Unverified          ▼  │            │
                         └──────────────► Holding{reason,      │
                                           since, last: h?}    │
                                               │ Unverified    │
                                               │ for 10 min    │
                                               ▼               │
                                           Degraded{since} ────┘
                                         (back to Agreed on agreement)
```

- `Single`: one node, nothing to compare. **No ceiling.** Same as today.
- `Checking`: more than one node, no outcome yet (just started, or a node
  was added). The ceiling stored from before (it survives restarts) stays,
  or is 0 if there was none.
- `Agreed { height, since }`: **the ceiling is `height`.**
- `Holding { hold, since, last_agreed }`: the hold is `Contested`,
  `Outvoted` or `Unverified`. **The ceiling stays at the last agreed
  height**, or holds everything (0) without one.
- `Degraded { since }`: no second node has answered for `UNVERIFIED_GRACE`
  (10 minutes). Rather than hold every payment for as long as fallbacks
  are down, **the ceiling is lifted** (the same trust as a single node),
  with a warning and a status line. A `Contested` or `Outvoted` hold, where
  a node actively disagrees, is never lifted this way.

`Effects` holds what a transition does to the stored ceiling: `Keep`,
`Set(h)`, `KeepOrHoldAll` (keep it, or 0 if there is none) or `Clear`.
Which nodes are left out is a second pure function,
`next_excluded(excluded, input)` (below). Both are applied in one place
(`agreement::apply`): the ceiling first, and if that write fails nothing
else changes and the check is retried. A state change is logged.

The states carry their data, so a ceiling without a height can't be
written.

### The ceiling: settlement only on agreed blocks

`settlement_ceilings(network, height)` is one row per network; no row means
no ceiling. `recompute_order_status` reads it beside `settlement_frozen`.
`plan_status` derives the status twice: from the real confirmation counts,
and from counts as of the ceiling. If only the first is a settlement
(`paid`/`overpaid`), settlement is deferred, exactly as it is during a
reorg: the order shows `confirming` with its real counts and is looked at
again next round.

So **an order can only become paid on blocks a majority of nodes has**:
the payment's block, and enough agreed blocks above it. The extra wait is
the time to the next check (one round, at most 30 s), plus a block if a
node is behind.

A rewind (`Store::forget_scanned_blocks_at_or_above`) lowers the ceiling
to below the fork in the same transaction, so a height agreed on a chain
that has since been replaced never vouches for the blocks that replace it.

### Exclusion: the majority's node gets pinned

`FallbackDaemonClient::set_excluded(nodes)` leaves nodes out of
`attempt_order`, so `pin()` and failover pick a majority node. The existing
reorg detection then finds where our recorded chain leaves that node's,
opens a reorg job, and reconciles the payments. Excluded nodes are also left
out of the two "ask every node" calls the reconciliation relies on
(`locate_transaction_corroborated`, `is_key_image_spent_corroborated`).
Without that, the lying node's "it's in block 3" put the made-up payment
straight back at its made-up height, on the real chain's block 3.

`next_excluded` keeps each excluded node with the lowest height it was
caught at, and **lets it back in only on evidence**: it has the
majority's block at or above that height. Two things don't count, both
found by the tests:

- saying nothing: a node serving a shorter made-up chain abstains on the
  real tip;
- agreeing below where it was caught: every chain shares the blocks before
  its fork, so after the rewind the liar "agrees" at the fork's parent.

Releasing on either made the engine pin the liar again, record its chain,
reorg back, and so on. Settlement stayed held throughout, but the loop was
wrong.

Exclusion never covers every node (`set_excluded` refuses, and nothing
changes). The node list changing (saved settings build a new client)
starts the exclusions afresh.

### When checks run

A check runs at the start of each round (`work::run_round`, before the
tiers) on a network with more than one node. It runs every round while
anything is unsettled: nothing decided yet, settlement held, or the agreed
height below our recorded top (a node catching up). Then a hold ends, and
the ceiling rises, the round the nodes agree. Once agreed up to our top, it
runs again when the recorded chain grows or after 30 seconds
(`RECHECK_SECS`). Each node gets 5 seconds per question and the whole
check 12. A check that can't finish (the database fails, or time runs out)
leaves everything as it is. The fast mempool pass doesn't check; it
settles through the same `recompute_order_status`, so the ceiling applies
to it too.

### Seeing it

`/status` gives each network's `agreement` (absent with one node): the
state, the hold, since when, the ceiling, the last check and the block it
decided on, and each node's vote there and whether it is excluded. monokulo
shows admins a "Node agreement" section per network: a "settlement held" or
"agreed" tag with one sentence on where it stands, and a table of nodes,
votes and exclusions. Anyone else sees none of it, as node addresses can
name internal hosts.

## Testing

1. **Pure functions, exhaustively** (`work::agreement::tests`).
   - `verdict` over every vote mix (4 kinds) for 1 to 5 nodes, against the
     rules, plus a table of named cases.
   - `start_height`: a node behind, our record lower, a chain served far
     ahead, too few answers.
   - `assess`: fresh fork (agreed one below, the node with the lost block
     off chain), lagging node, a silent node (neither on nor off), deep
     divergence (outvoted), even split (contested), nobody else answering.
   - Every `(state, input)` transition, the 10-minute grace, holds that
     keep their start, and the return from `Degraded`/`Holding`.
   - `next_excluded`: caught, not released by silence or by agreement
     below its catch height, the lowest catch height kept, released on
     evidence, cleared with one node.
2. **Status planning** (`store::tests`). `plan_status` with a ceiling
   defers exactly the settlements that need blocks above it; the counts
   shown stay real; a settled order isn't walked back; 0-conf acceptance
   is unaffected.
3. **Store.** The ceiling is written as decided; a rewind lowers it; no
   row means no ceiling.
4. **Fallback client.** Excluded nodes are never pinned or tried, even
   with every other node down; never every node; exclusion can be lifted.
5. **End to end with fake nodes** (`work::tests`):
   - three nodes, the primary serving a made-up block that pays an order
     plus ten made-up confirmations: the order never settles in any round;
     the primary is excluded within three rounds; the rounds after pin an
     honest node, reorg, and take the payment out of the block; then the
     real chain grows past ten confirmations of that height and the order
     still doesn't settle, the ceiling follows the real tip, and the liar
     stays out;
   - two nodes that disagree: `confirming` with eleven confirmations, the
     ceiling at the last shared block, nobody excluded; once they agree
     the order settles within a round;
   - a node a block behind: the ceiling is its height, settlement waits
     for that block and comes the round it arrives;
   - a single node: settles as before, no ceiling, `Single`.
6. **monokulo.** The section is shown to admins only, each state reads as
   one sentence.

## Not covered

- **0-conf.** A pool transaction is only ever the node's word. A store
  accepting 0-conf trusts its pinned node. Confirming a pool transaction
  with a second node is possible later (the pool poll is cheap).
- **Every node lying together.** No majority check helps; proof-of-work
  verification would.
- **A liar the majority can't outvote.** With two nodes, a lying fallback
  that disputes the real chain holds settlement for as long as it does
  (the status page names it; the operator removes it). Safety over
  liveness, deliberately. A lying node that isn't outvoted is also still
  asked where transactions are; `locate_transaction_corroborated` takes the
  most positive answer ("in a block" over "nowhere"), so such a node can
  keep a dropped payment at a height. The ceiling doesn't catch that: it
  vouches for blocks, not for which block holds a transaction. Checking a
  claimed location against the block itself (from the pinned node) would.
- **Hiding.** A node can stay on the majority's chain and still leave a
  transaction out of a block it serves. The block's id commits to its
  transactions, and the engine checks that ids match (`ChainBlock::txid`),
  so it can't drop one silently. A node that serves a stale chain is
  outvoted once that chain falls `WINDOW` blocks behind.

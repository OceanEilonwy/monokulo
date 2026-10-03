# Engine page: a live view of the scanner

Status: design, for review. Nothing here is implemented yet.

The animated mockup that goes with this document is
`docs/engine-visualizer-mockup.html` (open it in a browser; it plays a
scripted session against fake data, including a catch-up, a mempool payment
settling and a reorg).

## Purpose

The status page answers "is the engine healthy". The engine page answers
"what is the engine doing right now, and why". It is for operators (and for
us, while tuning the scheduler): it makes the scheduler described in
`docs/scanner_microtasks.md` visible, so you can watch:

- blocks arriving from the node, being fetched into the cache, scanned for
  each group of stores, and committed;
- each round's five tiers taking their share of the round, unit by unit,
  and how each tier ended (idle, backlogged, waiting and why, failed);
- what is queued for each tier and what gets picked off;
- where progress is saved (durable, in SQLite) and what lives only in memory;
- how far behind each group of stores is, and how fast it is catching up.

Everything moves on screen when it moves in the engine, so a payment can be
followed from the pool, through settlement, to its webhook; and a block from
the node, through the cache, into the stores' cursors and the database.

## What the engine does, and what is shown for it

This section is the inventory: every moving part of the scanner that the
page draws, with where it lives in the code. Anything not in this list is
deliberately left out (see "Left out").

### Per network, two loops

| Engine | Shown as |
| --- | --- |
| `loops::run_scanner_loop`: a round, then sleep for the poll interval, or start again at once when the round was backlogged, or wake early on a ZMQ block announcement | Round ribbon: one bar per round, gaps for sleeps, each gap marked with what ended it (timer, node announced a block, backlog) |
| `loops::run_fast_mempool_loop`: every 250 ms, scan new pool transactions against every store in scope | Mempool panel: a heartbeat tick each pass; a scanning sweep only when a pass found new transactions |
| Store key registration retries | A line in the stores summary ("2 stores waiting for their keys to register") |

### A round (`work::run_round`)

| Engine | Shown as |
| --- | --- |
| Budget (10 s, raised for large blocks), tier shares 20/40/15/20/5 % | Round lanes: one lane per tier, the reserved share drawn as a bracket where the tier actually started |
| Pass 1 (each tier up to its share), pass 2 (leftover time, priority order) | Units in pass 2 are drawn hatched |
| Progress floor: a tier with work always completes one unit, even over its share | A unit that ran past its share shows the overrun past the bracket's end |
| `Progress`: Advanced, Idle, Blocked(`Wait`), Failed | Each unit's bar; the tier's ending as a chip at the lane's end (Idle, Backlogged, Waiting: reason in words, Failed: error) |
| `RoundReport::backlogged` | The ribbon shows the next round starting at once, marked "backlog" |

### Blocks tier (`work::blocks`)

| Engine | Shown as |
| --- | --- |
| Node tip, high-water mark (`max_scanned_height`), recorded block window (pruned below the reorg window) | The chain strip: one cell per block, the node's tip on the right, the high-water mark and the reorg window marked above |
| Groups of stores at the same cursor; the frontier group at the high-water mark | Group pills under the strip at their cursor, frontier in the accent colour; the pill says how many stores |
| Catch-up rotation (`Rotation`, persisted as `scheduler_positions.catch_up_group`); the frontier/catch-up turn | A small pointer under the group that is served next; the turn alternates visibly |
| Big groups paged by 256 (`group_page`), a unit scanning up to 8 blocks (`blocks_per_unit`) | A pill over 256 stores is segmented by page; the page being scanned is filled |
| Idle stores moved straight to the high-water mark (`advance_idle_cursors`) | Those stores split off the pill and jump along the strip to the frontier |
| Block cache within the scan memory budget, prefetch overlapped with the scan, carried to the next round from the same node | Cells held in the cache are tinted; a fetch is a packet from the node card to a run of cells; a "cache" gauge shows bytes against the budget; a carried cache is shown by the cells staying tinted across the round boundary, a dropped one by them fading |
| Header-only recording of blocks nobody is scanned for | Small hollow cells, recorded in runs, without the scan animation |
| A block scan: per-tenant view-key scans in runs of 32 transactions, paged for large blocks | The scanning cell fills left to right with its transaction progress; the group pill pulses |
| Checkpoint of an interrupted block (`partial_block_*`) | A save marker on the half-filled cell: "saved at 1,200 of 3,000 transactions" |
| Commit: cursors moved, matches promoted, hash recorded, one transaction | The pill moves one cell right; the cell gets its recorded state; a save pulse goes to the database panel |
| `Diverged` | The cell turns to the reorg colour with "doesn't extend the recorded chain" and the frontier stops |
| First-run seed just below the tip | The strip starts with one recorded cell |
| Slow block, headers-first mode | Banner over the strip, same sentences as the status page |

### Chain tier (`work::chain`)

| Engine | Shown as |
| --- | --- |
| Detection: one compare at `min(high-water, tip)`, free when the tip hash came with the height; binary search on a mismatch | A probe that touches the compared cell ("agrees", or "free: the tip hash matched"); on a mismatch the probe hops through the search |
| Durable reorg job: Collect, Process (Keep, Move, Restore, Void per payment), Rewind | Reorg panel: four stations, the candidate count, a counter per decision, retrying candidates with their wait |
| While a job is open: blocks suspended, settlement frozen for paid/overpaid | Block pills greyed with "waiting for the reorg"; a "settlement held" chip on the settlement panel |
| Rewind: delete losing hashes, re-anchor, clamp cursors | The losing cells drop out of the strip; pills slide back to the fork; replacement cells arrive next round |

### Mempool tier and fast path (`work::mempool`)

| Engine | Shown as |
| --- | --- |
| Pool txids (polled with the tip), remembered bodies and per-store scans | The pool: one dot per transaction (up to 200, then a count), dimmed once every store has been scanned for it |
| Fast pass: new transactions only, up to its scan budget, the rest deferred to the round | New dots drop in from the node; a sweep scans them; deferred ones keep a ring until the round's rotation reaches them |
| A match: payment recorded and its order recomputed in the same job | The dot turns into a payment token and flies to settlement, then on to webhooks |
| A transaction mined | Its dot flies to the block that holds it |
| Not watching (nothing in scope) | The pool is drawn empty with "not watched: no store is waiting for a payment" |

### Settlement tier (`work::settlement`)

| Engine | Shown as |
| --- | --- |
| Obligations (`pending_payment_recomputes`) | A queue of tokens |
| Due orders by time and by height (`orders.next_due_*`) | A second queue, split by time and height |
| A recompute page (64, in database jobs of 16), each order once per round | Tokens leave the queues into the recompute box in batches |
| Status transitions | A chip in the order-state colours (`.state-*`): "confirming to paid" |
| Webhook enqueued with the transition | An envelope token flies to the webhook panel |
| Vanished-payment page and its backoff | A line: "12 unconfirmed payments looked at, 1 not found (next look in 16 s)" |
| Order backoff | "2 orders waiting to retry" |

### Upkeep tier (`work::upkeep`)

Four rows, each lit when it runs: pruning (rows removed), WAL checkpoint
(every ten minutes, with time to the next), void recheck (page position),
scanned ranges (page position).

### Shared infrastructure

| Engine | Shown as |
| --- | --- |
| Database worker: three classes (Scanner, Webhook, Admin), 64 slots each, served round-robin by one thread | Three short queues feeding one worker; a pointer steps round-robin; depth and the longest wait |
| Durable state, the "save states" | A "Saved" column listing what is on disk: store cursors, recorded blocks, block checkpoints, the reorg job, the scheduler positions, pending recomputes, webhooks due. Each row flashes when written |
| In-memory state | A "Memory" column beside it: block cache, carried cache, pool bodies, retry delays, rotation offsets. Its heading says "lost on restart; rebuilt" |
| Nodes: active and pinned for the round, fallbacks, cooldowns, RPC calls | Node cards on the right of the chain strip; each call is a short packet labelled with its method; a node in cooldown is greyed with its time left |
| ZMQ announcements | A spark on the node card when it announces a block or a transaction, with the wake it caused |
| Webhook delivery (batches of 50, woken by the scanner) | Envelopes waiting; a batch leaving; failures returning to wait |

### Overall progress

The summary row at the top: node tip, scanned to, blocks behind and the time
to catch up at the recent pace (the existing `NetworkScaling` figures), what
sets the pace (caught up, link, memory, CPU, round), stores (at the tip,
catching up, waiting and why), blocks a minute, and the last round's length
against its budget.

### Left out

- Store, order and payment identities, amounts and addresses. The page shows
  counts, heights, block hashes and txids (shortened; both are public chain
  data), never whose they are.
- Individual SQL statements and individual database jobs: the database panel
  shows queue depths and commits, not every job.
- Key custody internals beyond "scanning" and per-store retry delays.

## How it looks

The page is `/status/engine` in monokulo. One network at a time, chosen by
tabs when more than one is configured. Top to bottom:

1. **Summary row.** Six figures, each with a state chip where it has one.
2. **Chain strip** (the centre of the page). Blocks as cells along a height
   axis, node cards at the right edge, group pills below. When the span from
   the lowest cursor to the tip is more than about 60 blocks, the middle is
   cut with an axis break labelled with the blocks it hides, so the cells
   near the cursors and the tip keep their size.
3. **Round.** Five lanes for the current round, its playhead, and the round
   ribbon underneath (the last 40 rounds).
4. **Work.** Four panels side by side: Reorg, Mempool, Settlement, Upkeep.
5. **Plumbing.** Database worker, Saved and Memory, Webhooks.
6. **Events.** The same events as text, newest first, filterable by tier:
   the accessible equivalent of the animation, and what you read when you
   pause.

At phone width the panels stack in the same order and the chain strip
scrolls sideways inside its own box.

### Colour

Colour means one thing: **which tier**. The five tiers take the reference
categorical slots in tier order (Chain blue, Blocks orange, Mempool aqua,
Settlement yellow, Upkeep magenta), stepped separately for dark mode, and
validated with the dataviz palette checker against the card surface in both
themes. In light mode three of them are under 3:1 against the card, so
nothing is told by colour alone: every lane, token and chip also carries the
tier's name or icon, and the event table is always there.

Everything else uses the existing theme roles: order transitions use the
`--state-*` colours, failures `--error`, waits `--warning`, the frontier the
accent. The new colours become roles in `views/theme.css`
(`--viz-tier-chain` and so on, and `--viz-cell-*` for block states), so
`views::theme_tests` covers them like every other colour.

### Motion

- **Playback, not real time.** The browser plays events back about 1.5 s
  behind the engine ("Live, 1.5 s behind"), so a burst that arrived in one
  poll is played out in order and smoothly.
- **A time lens.** A caught-up round takes tens of milliseconds and then the
  loop sleeps for seconds. Shown in real time it would be a flicker and a
  long pause. So: each animation has a minimum length (a unit bar 150 ms, a
  token flight 600 ms); sleeps longer than a second are shortened to one
  second and labelled with their real length; the round lanes are scaled to
  the round's real length, with its budget shown as a gauge beside them
  ("0.42 s of 10 s").
- **Catching up.** If the playback falls more than 5 s behind, it speeds up
  (2x, then 4x, shown as a badge). More than 30 s behind, it jumps to the
  latest state and says how many events it skipped.
- **Merging.** Runs of the same thing become one animation: 300 header-only
  blocks are one sweep, not 300; fast passes that found nothing are a
  heartbeat; more than 50 tokens in flight merge into counted ones.
- **The snapshot is the truth.** Every poll carries a snapshot of the state.
  When the animations for a poll finish, the scene is set to that snapshot,
  so it never drifts from the engine.
- **Pause and step.** Pause stops the playback (events keep being buffered,
  up to a limit); Step plays one event; "Back to live" jumps to now.
- **Reduced motion** (`prefers-reduced-motion`): nothing travels; states
  cross-fade in 150 ms. The event table carries the flow.
- **A hidden tab** stops animating and, when shown again, jumps to live.

## Where the data comes from

### Engine: an activity recorder

A new module, `crates/engine/src/activity.rs`, with one `Activity` per
network. It lives where `progress` and `wakes` already live
(`scanner_status::NetworkScanStatus`, handed to `ScanState` by
`with_activity`, as `with_progress` and `with_wakes` are), so the loops write
it and the admin API reads it.

- A ring of the last 4,096 events, each with a sequence number and the
  engine's time in milliseconds.
- **Recorded only while watched.** A read marks the network watched for the
  next 30 s; outside that, recording is one atomic load and a return. So an
  engine nobody watches pays nothing.
- Typed events (`shared::activity`, used by both crates, as
  `shared::scaling` is):
  - `round_started { round, budget_ms, woken_by }`, `round_finished { round,
    ms, steps, outcomes, backlogged }`, `sleeping { until_ms }`
  - `unit { round, tier, pass, started_ms, ms, progress }` (one per unit,
    sent when it ends)
  - `rpc { node, method, ms, ok, bytes }` from the pinned client
  - `fetched { from, count, bytes, ms, prefetch }`, `cache_dropped { heights,
    unscanned_bytes }`
  - `block_scan { height, group, stores, page, txs }`, `block_progress {
    height, done_txs }` (at most one per call of 32 transactions),
    `checkpointed { height, stores, done_txs }`, `committed { height,
    hash, stores_moved, idle_moved, matches }`, `headers_recorded { from, to }`,
    `diverged { height }`, `idle_advanced { from, to, stores }`
  - `chain_checked { height, free }`, `fork_found { height }`,
    `reorg { phase, candidates, keep, moved, restored, voided, retrying }`,
    `rewound { fork }`
  - `pool { size, new, deferred, path }`, `tx_matched { txid, path }`,
    `tx_mined { txid, height }`
  - `recomputed { orders, transitions: [{ from, to, count }] }`,
    `vanished { looked, unresolved }`, `webhooks_enqueued { count }`
  - `upkeep { pruned, checkpointed, voids_rechecked, ranges }`
  - `webhooks_sent { sent, failed }`
- No store, order or payment ids, no amounts. Block hashes and txids are
  shortened to 8 characters.

### Engine: the endpoint

`GET /api/v1/admin/engine/activity?network=stagenet&after=<seq>` (engine
token, like every engine route; monokulo is its only caller):

```json
{
  "network": "stagenet",
  "epoch": "6f2c…",
  "now_ms": 1790000000123,
  "tuning": { "round_ms": 10000, "shares": [20, 40, 15, 20, 5],
              "group_page": 256, "blocks_per_unit": 8,
              "reorg_check_depth": 20, "poll_ms": 1000, "fast_ms": 250 },
  "snapshot": { "chain": {}, "groups": [], "cache": {}, "reorg": null,
                "mempool": {}, "settlement": {}, "upkeep": {}, "db": {},
                "saved": {}, "memory": {}, "webhooks": {}, "nodes": [],
                "loop": {} },
  "events": [ { "seq": 812, "at_ms": 1790000000010, "kind": "committed", "...": "..." } ],
  "next": 813,
  "gap": false
}
```

- `epoch` changes when the engine restarts, so the page knows to start over.
- `gap` is true when `after` has already left the ring: the page jumps to the
  snapshot.
- The snapshot's in-memory parts are read from the recorder and the
  `ScanState`; its database parts (groups and their sizes, queue lengths,
  checkpoints, the reorg job, scheduler positions) are read on the read pool,
  never the worker, at most once a second however many pollers there are,
  and capped (the 32 groups nearest the frontier, then "and 14 more").

### Monokulo: relay and page

- `GET /status/engine`: the page. **Admins only** (it shows node addresses
  and the shape of every store's progress); the status page links to it for
  admins, beside each network's "Chain scanner" heading.
- **Without JavaScript** it is a point-in-time page, rendered in Rust: the
  summary row, the chain strip and group pills as static HTML, each panel's
  figures, and the latest events table, with a Reload button (as agreed for
  every page but the checkout embed).
- `GET /status/engine/events`: an SSE stream for the page's script. One
  shared poller per network runs while anyone is watching: it asks the
  engine every 500 ms, and sends each viewer the same messages, so ten open
  tabs cost the engine one request every 500 ms. A new viewer gets the
  latest snapshot and the last 200 events first. Streams count against the
  abuse stream limit like the status page's.
- **Rust writes every word.** Each event the relay sends carries its
  sentence (`"Block 3,412,881 committed for 41 stores"`) and formatted
  figures, written by monokulo; the script only places and moves things.
- **The script** is `static/engine-view.js`: plain JavaScript, no library,
  drawing SVG from the snapshot and animating it with the Web Animations
  API. SVG rather than canvas so it takes the theme's roles through CSS and
  stays sharp, with a budget (about 120 cells, 200 pool dots, 50 tokens) that
  keeps it light.

## Decisions to review

1. **A JSON stream and a script for the animation.** Our rule is that
   rendering stays in Rust and pages send HTML fragments. Animating flow
   needs the browser to know what moved where, so this page streams JSON
   events with Rust-written text, and its script draws the scene. The no-JS
   page is still rendered entirely in Rust. Alternative: Rust renders the SVG
   scene and streams it as fragments through ssexi, with CSS transitions
   between swaps; that animates state changes but can't fly tokens between
   panels, so most of the "flow" is lost. Recommended: the script.
2. **Admins only.** Alternative: public like `/status`, with node labels
   hidden. Recommended: admins only, as the abuse and announcement sections
   already are.
3. **Recording only while watched.** Costs nothing unwatched, but the first
   view starts with a snapshot and no history. Alternative: always record
   (the ring is small, and events are per unit, not per transaction).
   Recommended: only while watched.
4. **Plain JavaScript, no library.** A charting or animation library would
   save little here and add a dependency we would vendor. Recommended: none.

## Work, once approved

1. `shared::activity` types; `engine::activity` recorder with its tests
   (ring, watched window, gap and epoch).
2. Recording in `run_round`, the loops and each tier, with tests that run
   real rounds on the existing work fixtures and check the event sequence
   (a catch-up, a mempool payment, a reorg).
3. The snapshot and the admin endpoint, with its tests.
4. Monokulo: client call, relay poller, `/status/engine` no-JS page, admin
   gate, status page link, theme roles.
5. `static/engine-view.js`: scene, playback, time lens, reduced motion.
6. Browser coverage: page loads, plays a scripted session from a fake
   engine, screenshots in the coverage gallery in light and dark.

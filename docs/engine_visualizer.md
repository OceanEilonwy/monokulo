# Engine page: a live view of the scanner

Status: implemented. The design below was reviewed and approved; how it was
built, and the decisions made while building it, are at the end and in
[engine_visualizer_decisions.md](engine_visualizer_decisions.md).

The animated mockup that goes with this document is
`docs/engine-visualizer-mockup.html` (open it in a browser). A fake
engine runs behind it, starting with 25 minutes of history (a store catching
up 20 minutes ago, a payment 13 minutes ago, a reorg 5 to 6 minutes ago);
three buttons make the same things happen now.

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
| `loops::run_scanner_loop`: a round, then sleep for the poll interval, or start again at once when the round was backlogged, or wake early on a ZMQ block announcement | Round ribbon: one bar per round, coloured by tier. A sleep is a dotted gap; a sleep cut short by the node announcing a block ends in a small block outline (the same shape as a chain cell); a round that started at once because work was left has no gap |
| `loops::run_fast_mempool_loop`: every 250 ms, scan new pool transactions against every store in scope | Mempool line: a heartbeat each pass; new transactions appear as dots, filled once scanned |
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
| Commit: cursors moved, matches promoted, hash recorded, one transaction | The pill moves one cell right; the cell gets its recorded state; a save square goes to the Database line |
| `Diverged` | The cell turns to the reorg colour with "doesn't extend the recorded chain" and the frontier stops |
| First-run seed just below the tip | The strip starts with one recorded cell |
| Slow block, headers-first mode | Banner over the strip, same sentences as the status page |

### Chain tier (`work::chain`)

| Engine | Shown as |
| --- | --- |
| Detection: one compare at `min(high-water, tip)`, free when the tip hash came with the height; binary search on a mismatch | A probe that touches the compared cell ("agrees", or "free: the tip hash matched"); on a mismatch the probe hops through the search |
| Durable reorg job: Collect, Process (Keep, Move, Restore, Void per payment), Rewind | Reorg line, opened by itself: four stations, the candidate count, a counter per decision, retrying candidates with their wait |
| While a job is open: blocks suspended, settlement frozen for paid/overpaid | Block pills greyed with "waiting for the reorg"; "paid held: reorg open" on the Order status line |
| Rewind: delete losing hashes, re-anchor, clamp cursors | The losing cells drop out of the strip; pills slide back to the fork; replacement cells arrive next round |

### Mempool tier and fast path (`work::mempool`)

| Engine | Shown as |
| --- | --- |
| Pool txids (polled with the tip), remembered bodies and per-store scans | The pool: one dot per transaction (up to 200, then a count), dimmed once every store has been scanned for it |
| Fast pass: new transactions only, up to its scan budget, the rest deferred to the round | New dots drop in from the node; a sweep scans them; deferred ones keep a ring until the round's rotation reaches them |
| A match: payment recorded and its order recomputed in the same job | The dot turns orange; an envelope flies to the Webhooks line |
| A transaction mined | Its dot flies to the block that holds it |
| Not watching (nothing in scope) | The pool is drawn empty with "not watched: no store is waiting for a payment" |

### Settlement tier (`work::settlement`), shown as "Order status"

This tier is where what the chain and the pool say becomes what the shop
sees: it recomputes each affected order's status (pending, unconfirmed,
confirming, paid, expired) and queues the shop's webhook in the same
transaction. Without it a payment would be found and recorded but no order
would ever change. It earns one line on the page because it answers "the
payment was seen, so why hasn't the shop heard?": a growing queue, a paid
status held during a reorg, or an order backing off. The panel is called
"Order status" (the tier keeps its name in the round lanes), and its detail
starts with that sentence.

| Engine | Shown as |
| --- | --- |
| Obligations (`pending_payment_recomputes`) and due orders (`orders.next_due_*`) | Summary: how many orders wait to be recomputed. Detail: the two queues as tokens |
| A recompute page (64, in database jobs of 16), each order once per round | Tokens leave the queues |
| Status transitions | Summary: the last state reached, as a `.state-*` chip. Detail: the last three, "confirming to paid" |
| Webhook enqueued with the transition | An envelope flies to the Webhooks line |
| A paid transition held during a reorg | Summary reads "paid held: reorg open" |
| Vanished-payment page and its backoff | Detail line: "12 unconfirmed payments looked at, 1 not found (next look in 16 s)" |
| Order backoff | Detail line: "2 orders waiting to retry" |

### Upkeep tier (`work::upkeep`)

Summary: four small squares (pruning, WAL checkpoint, void recheck, scanned
ranges) that light as each runs, and the round it last ran in. Detail: what
each did (rows removed, time to the next checkpoint, page positions).

### Shared infrastructure

| Engine | Shown as |
| --- | --- |
| Database worker: three classes (Scanner, Webhook, Admin), 64 slots each, served round-robin by one thread | One line: three tiny bars (queue depths) and "38 jobs a second, longest wait 1.7 ms". Detail: each queue's depth, whose turn is next, the longest job |
| Durable state (the "save states") | In place rather than in a panel: every save puts a small dark square on the thing saved (a block cell, a group pill, the reorg line, the order-status line) and sends it to the Database line. A block checkpoint keeps its square on the half-filled cell |
| In-memory state | In place too: the cache gauge on the chain, pool bodies on the mempool line, retry delays on pills and order status |
| Both, together | A "Restart safety" line: "60 saves a minute; 4 blocks and 8 pool bodies only in memory". Detail: what is saved and what is only in memory, side by side, each saved row lighting when written |
| Nodes: active and pinned for the round, fallbacks, cooldowns, RPC calls | Two small cards right of the chain strip; each call is a short packet labelled with its method; a node in cooldown is greyed with its time left |
| ZMQ announcements | A spark on the node card when it announces a block or a transaction |
| Webhook delivery | One line: deliveries a minute and a sparkline of the last five minutes (10 s buckets). Detail: due now, sent and failed in the last five minutes |

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
- Individual SQL statements and individual database jobs: the Database line
  shows queue depths and commits, not every job.
- Key custody internals beyond "scanning" and per-store retry delays.

## How it looks

The page is `/status/engine` in monokulo. One network at a time, chosen by
tabs when more than one is configured.

**It fits one screen.** The target is a 1920 x 1080 monitor (a viewport of
about 1920 x 960 in a browser). Everything that animates (timeline, summary,
chain, round) and every panel's one-line summary ends about 560 px from the
top, so it also fits a 1440 x 900 laptop and a 1366 x 768 one without
scrolling. Only the events table is below.

Top to bottom:

1. **Header** (one line): Status crumb, "Engine", network tabs. The app bar
   carries monokulo's System / Light / Dark toggle, as on every page.
2. **Timeline** (one line, see below): Pause or Play, Live, the track, and
   "Live, 1.5 s behind" or "Paused, 36 s behind live".
3. **Summary** (one line of six): node tip, scanned to, behind (and time to
   catch up), stores (and how many are catching up), the last round against
   its budget, what sets the pace.
4. **Two columns.**
   - Left, the animation: the **chain strip** (blocks as cells, the node
     cards beside it, group pills under it) and the **round** (five lanes,
     each 18 px high, with the reserved share, the units, the playhead and
     the outcome chip; under the lanes, the playhead carries the round's
     length so far as a label ("0.42 s"); the recent-rounds ribbon
     underneath). **The round card never changes size**: rows have fixed
     heights, an outcome chip fills a space that is always there, and the
     state line is one line that ellipsises.
   - Right, 380 px: **one line per part**, each opening for its detail
     (`<details>`, so it works without JavaScript too). Each line is a
     condensed summary that still moves, and the animations fly to it
     whether it is open or not:
     - Reorg: "Agrees at 3,412,882, free". Opens by itself, with a red edge,
       while a reorg job is open, and closes when it ends.
     - Mempool: a heartbeat for each fast pass, the newest transactions as
       dots, "8 in the pool, 1 payment found".
     - Order status: "2 to recompute", and the last state reached.
     - Upkeep: four squares lighting as each job runs.
     - Database: three queue bars and jobs a second.
     - Webhooks: deliveries a minute and a sparkline.
     - Restart safety: saves a minute and what is only in memory.
5. **Events**, below the fold: the same events in words, up to the playback
   position, filterable by tier; clicking a row moves the timeline there.

Cards are tight: 8 px padding, 8 px between them, 13 px text, 11 px labels.
Below 1150 px wide the right column moves under the animation as a grid of
the same lines; at phone width everything is one column and the chain strip
shows fewer cells.

### Timeline

One track across the top of the page, showing a **window** of the history
(the last five minutes at first).

- **Every event is a thin vertical line; key events are a circle** in their
  tier's colour (a payment found, a reorg found and rewound, a group of
  stores falling behind or catching up, a block checkpointed, a failure).
  New blocks are not key events: they come every two minutes and would
  crowd out everything else. Hovering a circle shows its sentence. The axis
  is labelled in time ago ("6 min ago").
- **Drag the track to move along the history**: drag right to go back in
  time, left to come forward. **Scroll to zoom** around the pointer (from
  5 s, about five rounds, to the whole history). While the window's right edge is at now it
  follows now; once moved into the past it stays there.
- **Click to move the playhead** (a press that doesn't move more than a few
  pixels is a click, not a drag). That pauses and moves the whole page to
  that moment: chain, round, panels and events table. Events after the
  playhead are drawn faded, and the stretch from the playhead to now is
  shaded. A playhead outside the window shows as an arrow at the edge it
  lies past.
- **A thin bar along the track's bottom edge** is the whole history (up to
  30 minutes): the window is marked on it, and the playhead as a tick, so
  you always know where in the history you are looking.
- **Play** replays at real speed from there, sliding the window along when
  the playhead reaches its edge, and turns live on reaching now; **Live**
  jumps to now and sets the window following again.
- **Keyboard:** the track is a slider. Left and right jump to the previous
  or next key event, with Shift to any event; Page Up and Page Down move
  along the history by half a window; plus and minus zoom; End goes live;
  Space plays or pauses.

How seeking works: the page keeps the events it has received and a copy of
its own state every 2 s (a keyframe). To show a moment it takes the
keyframe before it and applies the events up to it without animating, which
takes milliseconds. Playing forward from there animates again. This is the
only way the page's state changes: every event is applied to a model and
the view only draws the model, so a scrubbed view and a live one are drawn
the same way.

### Colour

Colour means one thing: **which tier**. The five tiers take the reference
categorical slots in tier order (Chain blue, Blocks orange, Mempool aqua,
Settlement yellow, Upkeep magenta), stepped separately for dark mode, and
validated with the dataviz palette checker against the card surface in both
themes. In light mode three of them are under 3:1 against the card, so
nothing is told by colour alone: every lane, token, circle and chip also
carries the tier's name, and the event table is always there.

Everything else uses the existing theme roles: order transitions use the
`--state-*` colours, failures `--error`, waits `--warning`, the frontier the
accent, saves `--ink`. The new colours become roles in `views/theme.css`
(`--viz-tier-chain` and so on, `--viz-cell-*` for block states,
`--viz-saved`), so `views::theme_tests` covers them like every other colour.

### Motion

- **Playback, not real time.** The page plays frames about 1.5 s behind the
  engine, so a burst that arrived in one poll plays out in order; a frame's
  effects play staggered by their own events' times.
- **Rounds to scale.** A caught-up round takes tens of milliseconds, so its
  lanes are scaled to its real length (with a floor of 120 ms), its length
  so far on the playhead's label. While a group is catching up or a reorg
  is open, the lanes are scaled to the full 10 s budget and show each tier's
  reserved share. The time before a round's first unit is the node
  answering the round's tip request, and is shown as it is.
- **Falling behind.** A hidden tab doesn't animate; with more than 40 frames
  waiting, playback skips to the newest. Nothing is lost: the timeline
  still holds every mark, and scrubbing rebuilds any moment.
- **Reduced motion** (`prefers-reduced-motion`): nothing travels; states
  change in place. The timeline and the event table carry the flow.

## How it is built

The decisions behind each part, and what else was possible, are in
[engine_visualizer_decisions.md](engine_visualizer_decisions.md).

### Engine: the activity record

`engine::activity::Activity`, one per network, kept in
`scanner_status::NetworkScanStatus` beside `progress` and `wakes` and handed
to `ScanState` by `with_activity`; `TestEngineHandle::activity` reaches it
in tests.

- A ring of the last 30 minutes of events, at most 50,000, each with a
  sequence number and the engine's time in milliseconds. Always recording:
  events are per unit, per block and per payment, never per transaction or
  per SQL statement.
- A **snapshot every 10 s**, taken by the scan loop before a round
  (`work::snapshot`): one store call for the database facts
  (`Store::activity_facts`, each query indexed; migration 0025 indexes
  webhook deliveries by when they were made) and the scheduler's memory
  (the carried block cache, the pool it remembers, the database worker's
  queues, the nodes).
- The events (`shared::activity::Event`): `round_started` (with the tip it
  read), `unit` (tier, pass, start, length, progress), `tier_ended`,
  `round_finished`, `slept` (and what woke it); `chain_checked`,
  `reorg_found`, `reorg_collected`, `reorg_processed`, `reorg_rewound`;
  `seeded`, `fetched`, `block_scan_started`, `block_progress` (pages of a
  large block), `checkpointed`, `committed` (stores moved, payments found,
  idle stores moved with it), `diverged`, `idle_advanced`; `pool_scanned`
  and `tx_matched` (fast path or round); `recomputed` (with the status
  changes), `vanished`; `upkeep`. No store, order or payment ids, no
  amounts; block hashes and txids shortened to 8 characters.

`GET /api/v1/admin/engine/activity?network=stagenet&from=<seq>` (engine
token) returns `shared::activity::ActivityPage`: the events from `from` on,
or, without it (or when it has left the ring, or belongs to another epoch),
everything from the oldest snapshot, flagged as a `gap`; the record's
`epoch`, the engine's clock and the scanner's tuning. Memory only.

### Monokulo: the state machine, the history, the relay

- **`engine_view::machine`** is the page's logic: `step(state, event)` gives
  the next state, the effects that lead to it (a call to the node, a token
  flying, a save, a flash, blocks arriving or dropping) and a mark (the
  event as a sentence; key or not). No I/O, no clock, no drawing. A
  snapshot overrides the events' account; between snapshots the state
  follows the events (groups of stores keep their identity as they move).
- **`engine_view::present`** turns a state into the page's words and
  figures (`Presented`). Every word on the page is written here.
- **`engine_view::history`** keeps a network's events for 30 minutes with
  the machine's state every 5 s: the live state, the state at any moment,
  replay frames (500 ms apart, at most a minute per request).
- **`engine_view::relay`** polls the engine every 500 ms for each network
  someone watches (and for a minute after the last viewer leaves), feeds
  the history and sends every viewer the same frames.

### Monokulo: the page

- `GET /status/engine?network=` (admins only; the status page links admins
  to it, "Watch it live", beside each network's Chain scanner heading):
  the whole page rendered in Rust from the live `Presented`, and the last
  60 marks. Without JavaScript that is the page, with a Reload button.
- `GET /status/engine/events?network=`: a `history` event (the live frame,
  every mark kept, the engine's clock), then a `frame` event per change,
  `restarted` (the script reconnects) and `unreachable`. Counted against the
  abuse stream limit.
- `GET /status/engine/at?network=&ms=` and
  `GET /status/engine/replay?network=&from=&to=`: what the page drew at a
  moment, and the frames between two.
- `static/engine-view.js`: plain JavaScript, no library. It plays frames,
  animates their effects (Web Animations), and runs the timeline on a
  canvas. It formats nothing but the timeline's "3 min ago" marks.
- The page's styles are `views::engine`'s `ENGINE_STYLE`, checked by
  `views::theme_tests`; its colours are roles in `views/theme.css`
  (`--viz-tier-*`, `--viz-cell-*`, `--viz-saved`).

### Tests

- Engine: the ring (bounds, gaps, epochs, snapshots); the paid story (pool,
  mined, reorged, mined again, confirmed) recorded in order inside its
  rounds; a big block's checkpoints and commit; idle stores moving on; the
  fast path's match and status change; the loop's snapshot and its sleep
  cut short by a new block; the snapshot after the story; the store facts
  counted once per network, with their query plans checked; the endpoint.
- Monokulo: the machine, event kind by event kind, and a 20,000-step run of
  events in any order against its invariants; the presentation's words; the
  history against brute force (any moment, before and after trimming,
  replay frames); the relay and the page's routes against the real engine
  router; the status page's link.
- Browser (`e2e/pos-playwright/tests/coverage-engine.spec.js`, in the
  browser coverage suite): a scripted story played into the fixture
  engine's record, followed live; a click on an event, the keys, replay,
  zoom and drag, a filter; the page without JavaScript; the admin gate;
  gallery shots in both themes.

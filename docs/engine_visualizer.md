# Engine page: a live view of the scanner

Status: design, for review. Nothing here is implemented yet.

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

- **Playback, not real time.** The page plays events about 1.5 s behind the
  engine, so a burst that arrived in one poll plays out in order.
- **A time lens.** A caught-up round takes tens of milliseconds and then the
  loop sleeps for seconds. So each animation has a minimum length (a unit
  bar 150 ms, a token flight 600 ms); sleeps longer than a second are
  shortened to one second on screen and labelled with their real length;
  the round lanes are scaled to the round's real length, with the length so
  far on the playhead's label. While a group is catching up, the lanes are
  scaled to the full 10 s budget and show each tier's reserved share.
- **Catching up.** If live playback falls more than 5 s behind, it speeds
  up (2x, then 4x, shown as a badge); more than 30 s behind, it jumps to
  now. Nothing is lost: the timeline still holds every event.
- **Merging.** Runs of the same thing become one animation: 300 header-only
  blocks are one sweep; fast passes that found nothing are a heartbeat; more
  than 50 tokens in flight merge into counted ones.
- **Reduced motion** (`prefers-reduced-motion`): nothing travels; states
  cross-fade in 150 ms. The timeline and the event table carry the flow.
- **A hidden tab** stops animating and, when shown again, jumps to live.

## Where the data comes from

### Engine: an activity recorder

A new module, `crates/engine/src/activity.rs`, with one `Activity` per
network. It lives where `progress` and `wakes` already live
(`scanner_status::NetworkScanStatus`, handed to `ScanState` by
`with_activity`, as `with_progress` and `with_wakes` are), so the loops write
it and the admin API reads it.

- A ring of the last 30 minutes of events (at most 50,000), each with a
  sequence number and the engine's time in milliseconds, so a page opened
  now can scrub back over what happened before it was opened.
- **A snapshot every 10 s** in the same ring, as an event: the in-memory
  state the page draws (groups, cache, the scan in progress, queues, the
  reorg job). It is where a page opened later starts its history; events
  alone can't say what the state was when they begin.
- **Always recording.** Events are per unit of work, per block and per
  payment, never per transaction scanned or per SQL statement, so a busy
  round adds a few dozen. Recording is a mutex push onto the ring.
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
- With no `after`, it returns the whole ring from its oldest snapshot, so a
  new page has the last 30 minutes to scrub through.
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
  history first (the ring, from its oldest snapshot), then live events. Streams count against the
  abuse stream limit like the status page's.
- **Rust writes every word.** Each event the relay sends carries its
  sentence (`"Block 3,412,881 committed for 41 stores"`) and formatted
  figures, written by monokulo; the script only places and moves things.
- **The script** is `static/engine-view.js`: plain JavaScript, no library,
  drawing the scene as HTML and SVG and animating it with the Web Animations
  API, so it takes the theme's roles through CSS and stays sharp; a budget
  (about 120 cells, 14 pool dots, 50 tokens in flight) keeps it light. The
  timeline alone is a canvas, since it may draw thousands of lines; it reads
  its colours from the same roles.

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
3. **Always recording, 30 minutes kept.** The timeline makes history worth
   having: an operator opens the page because something just happened.
   Alternative: record only while a page is open (free when nobody watches,
   but the timeline starts empty). Recommended: always, 30 minutes. This
   reverses the first draft, because of the timeline.
4. **Plain JavaScript, no library.** A charting or animation library would
   save little here and add a dependency we would vendor. Recommended: none.

## Work, once approved

1. `shared::activity` types; `engine::activity` recorder with its tests
   (ring, its time and count limits, snapshots, gap and epoch).
2. Recording in `run_round`, the loops and each tier, with tests that run
   real rounds on the existing work fixtures and check the event sequence
   (a catch-up, a mempool payment, a reorg).
3. The snapshot and the admin endpoint, with its tests.
4. Monokulo: client call, relay poller, `/status/engine` no-JS page, admin
   gate, status page link, theme roles.
5. `static/engine-view.js`: model, keyframes and seeking, scene, timeline,
   playback, time lens, reduced motion.
6. Browser coverage: page loads, plays a scripted session from a fake
   engine, screenshots in the coverage gallery in light and dark.

# Engine page: implementation decisions

The calls made while building the engine page (`docs/engine_visualizer.md`)
without the reviewer at hand, each with what else was possible and why this.
In the order they were made. The last section lists what the build does
differently from the approved design, and what is left for later.

## D1. One `Tier`, `Wait` and `TierOutcome`, in `shared::activity`

The engine's `work::Tier`, `Wait` and `TierOutcome` moved to
`shared::activity` and `work` re-exports them. The activity events carry
them, and monokulo reads them. Alternative: mirror types in `shared` with
conversions in the engine; two definitions of the same thing that could
drift. The engine's own uses are unchanged.

## D2. Events carry facts, monokulo writes the words

`shared::activity::Event` has heights, counts, tiers and outcomes, never a
sentence. Monokulo turns events into sentences, as it already does for
`/status`. Keeps "rendering in the presentation layer" and lets the wording
change without touching the engine.

## D3. Snapshots are taken by the scan loop, every 10 s, into the record

The engine's admin endpoint does no database work: it only reads the
in-memory record. The scan loop, before a round, records a `Snapshot` event
when one is due (`activity::SNAPSHOT_EVERY`): one store call
(`Store::activity_facts`, each query indexed) on the Scanner class, plus
in-memory state. Alternative: compute the snapshot per request; every viewer
would cost database work, and a snapshot taken on request can't sit in the
history as a point to rebuild from.

## D4. No unit-started event

Each unit is one `Unit` event when it ends, with its start offset and
length. A unit-started event would double the record's size for the most
common event. A long unit (a catch-up) is visibly running anyway: its block
events arrive while it runs.

## D5. Webhook rate from the database, per network

Webhook delivery is one loop for every network, so it can't record into a
network's activity. The snapshot counts deliveries made per 10 s over the
last five minutes for the network's stores, with a new partial index on
`delivered_at_utc` (migration 0025) so the query reads only those rows.
"Failed" was dropped from the panel's detail: due and sent are what the
design's one line shows, and failures have their own place (the logs).

## D6. The page's logic is a state machine in Rust, in monokulo

(Asked for by the reviewer.) `monokulo::engine_view::machine` is a pure
function: state and an event in, the next state and a list of effects out.
No I/O, no clock, no DOM. Every event kind and edge case is a unit test.

Where it runs: in monokulo, not the browser. Monokulo keeps each network's
history (the engine's 30-minute record) with a state keyframe every few
seconds, runs the machine, and sends the browser frames: the state, and the
effects to animate with their times. Scrubbing asks monokulo for the state
at a moment; replay fetches the stored frames. The browser script only draws
a state and plays effects.

Alternatives: the same machine in JavaScript, tested with `node --test` (no
JavaScript test runner exists in the repository, and the rule here is that
rendering and wording stay in Rust); or Rust compiled to WebAssembly (a
toolchain and a payload for one page). The cost of the server-side machine is
a round trip per scrub step: a few milliseconds for an admin page. The gain:
one implementation, tested in Rust, used both for the live page and for the
page without JavaScript.

## D7. The state machine changes the state in place

`step(&mut State, &Recorded) -> Output`, not `step(State, Event) -> (State,
Output)`. The same thing (one state in, the next out, nothing else touched)
without moving a few kilobytes per event; the history clones the state for
its keyframes, and tests compare states by value.

## D8. Frames carry the whole view, once per poll

What the browser gets per change is a frame: the full `Presented` view
after the poll's events, and the effects with their events' times, not a
state per event. A poll is every 500 ms, so the drawn state changes at most
twice a second; the effects, played at their own times, carry the movement
between. Sending a view per event would multiply the stream by the number
of events per poll for no visible gain.

## D9. A commit's smaller move goes first

A commit can move a group's scanned stores to the block and its idle ones
elsewhere (straight to the high-water mark when catching up). The machine
moves the smaller part first, so the larger part is what empties the old
group and keeps its identity: on the page, the pill slides rather than one
disappearing and another appearing. Found by the machine's tests.

## D10. The cached blocks follow the engine's own rule

After a commit, the machine lets go of cached blocks at or below every
group's cursor, as the engine's carried cache does. Without it the restart
panel counted blocks "only in memory" that the engine had already dropped,
until the next snapshot.

## D11. "Chain" replaces "Pace set by" in the summary

The pace comes from `/status`'s scaling figures, which the activity record
doesn't carry. Fetching `/status` too would cost a second request per poll
(it asks every node for its height). The sixth summary figure is the chain
check instead: agrees, differs, diverged, or reorg (from which block). The
pace stays on the status page, one click away.

## D12. Scrubbing asks the server, latest wins

A click or drag on the timeline asks `/status/engine/at` for that moment;
an answer for a moment no longer wanted is dropped. Replay asks for ten
seconds of frames at a time. Alternative: send the browser the whole
history's events and a copy of the machine; that is D6's JavaScript
alternative.

## D13. The timeline's marks are the sentences, not every event

The browser gets marks (an event as a sentence), not the raw events: they
are what the timeline draws and the table lists. Routine events have no
mark, so they don't crowd the timeline: a reorg check that agrees, a fast
pass that found nothing, a round ending, a sleep, a block fetched. Key
events (the circles): a payment found, a reorg found and rewound, a
checkpoint, stores catching up onto the frontier, the first run's seed, a
unit that failed or that the node failed, a payment voided, an order paid.
New blocks are a line, not a circle (approved in the design review).

## D14. The timeline zooms down to 5 s, not 20 s

The design said 20 s. With rounds about a second apart, 20 s is still a
blur of lines; 5 s shows single rounds. Found while writing the browser
test.

## D15. Rounds include the tip request (replaced by D21)

A round's lanes start at the round's start, and the round first asks the
node for its tip (and the pool) before any tier runs. Against a remote node
that is most of a caught-up round (0.4 s of 0.44 s on stagenet), so the
lanes' units sit at the right. That is the truth about where the time
goes, so it is drawn as it is rather than hidden.

## D16. The relay lingers a minute and then lets go

A network is polled while anyone watches it and for a minute after the last
viewer leaves, so a reload doesn't start the history over; then its
history is let go of, and the next viewer reads the engine's 30 minutes
again. A viewer's first read is the engine's whole record (up to about
1.5 MB of JSON on a busy network), once.

## D17. Webhooks due between snapshots are counted from recomputes

A recompute that changes an order's status queues its webhook in the same
transaction, so the machine adds one to "due" per status change; the next
snapshot sets the true figure (deliveries since then included).

## D18. The engine's internal changes

To record what the page needs, a few engine functions now say what they
did: `recompute_and_notify` returns the status change it made (or none);
`JobStep::Processed` carries how many candidates it examined and
`JobStep::Rewound` its fork; a block commit returns the payments found and
the idle stores moved; pruning returns the rows removed. `Db::queued` (was
test-only) and `QUEUE_CAPACITY` are public for the snapshot, and the block
cache's carried heights have a live accessor in place of the test-only one.
Each caller was updated; no behaviour changed.

## D19. Test helpers

`admin_session_cookie` existed twice (admin settings and invites tests);
it moved to `http::test_support` and the page's tests use it too.
`TestEngineHandle::activity` lets monokulo's tests (and the coverage
fixture) record into a real engine's record, so the relay, the routes and
the browser test run against the engine's real endpoint, not a stand-in.

## D20. No new settings

The poll (500 ms), the linger (60 s), the record's reach (30 minutes,
50,000 events), the snapshot interval (10 s) and the keyframe interval
(5 s) are constants with their reasons beside them, as the scanner's own
timings are. None is something an operator would tune, and each is
checked by a test at its production value.

## Second review

The reviewer's eight points after trying the page, and the calls made on
each.

## D21. A round's lanes start at its first unit

Replaces D15. Drawing the tip request on the lanes put every quiet round's
work at the far right, at a different place each round, which read as the
lanes jumping about. The lanes now start at the first unit; the state line
says how long the ask took ("Ended at 0.46 s (0.40 s of it asking the node
for its tip)"), so the time is still told. The playhead's label is the
tiers' time, in milliseconds under a tenth of a second ("4 ms"), since
"0.00 s" said nothing. Alternative: a grey "asking the node" segment at the
start of every lane; it would take most of the width in every quiet round
for the same information.

## D22. The node's whole pool, asked only while someone watches

The next block (the dotted cell after the tip) fills with the node's pool.
The engine never needed the pool's size, and it only reads the pool while
an order could be paid from it, so this is a new call: monerod's `get_info`
(pool size, median block weight) and `/get_transaction_pool_stats` (the
pool's bytes). It is asked every 5 s, and only while the activity record
has been read in the last minute: an engine nobody watches sends the node
nothing more than before.

- **Beside the round, not in it.** The scan loop spawns the call, so a slow
  node never delays a round, and it goes to the active node directly
  (`FallbackDaemonClient::pool_outlook`), so an answer or a failure never
  changes which node scans.
- **Bytes stand for weight.** The penalty is reckoned on weight, which
  monerod's pool stats don't give; bytes are close for ordinary
  transactions. The penalty-free zone is the median block weight, never
  under 300 kB (monerod's `CRYPTONOTE_BLOCK_GRANTED_FULL_REWARD_ZONE_V5`).
- **A node that refuses the stats** still gives the count from `get_info`:
  the cell shows the number with no fill.
- **Between askings** a scan of the pool (`pool_scanned`) updates the count
  and scales the bytes with it.

Alternative: the engine always asking. Rejected: the engine is careful to
ask the node nothing it doesn't need (the mempool tier's own rule), and the
page is the only reader.

## D23. "In the pool" says whose pool

"0 in the pool" counted the transactions the scanner remembers, and with no
order waiting the scanner doesn't look at the pool at all (by design: no
request is made). The snapshot now says whether the pool is looked at
(`Pool::watched`), and the line reads "23 in the node's pool, not scanned"
or "23 in the node's pool, 1 payment found"; the panel's rows separate the
node's count, the next block's fill, whether the engine scans, and what it
remembers.

## D24. The timeline's window has handles; the wheel scrolls the page

Replaces the wheel zoom and the drag-to-pan of the approved design. The
bar under the track (the whole history) carries the window as a box: its
middle moves it, its two handles resize it (5 s at least), a press beside
it centres it there. Pressing or dragging on the track itself moves the
playhead (scrubbing). Moving the window never moves the playhead or leaves
live. Scrubbing sends one request at a time: while one is out, the latest
position is noted and asked for next, so a fast drag doesn't queue
hundreds. The keyboard keeps left, right, End and Space on the track; the
window and its handles take Tab and the arrow keys. Page Up and Page Down,
plus and minus are gone (the window's keys do the same).

## D25. The legend is a `details`

The (?) beside the title is a `<details>`, so the legend opens without
JavaScript too; the script closes it on Escape and on a press outside. Its
words are written in Rust with the rest of the page, and its symbols are
the page's own classes, so the legend can't drift from what it explains.

## D26. Smaller wording calls

- The reorg check's packet reads "hash check", not "hash": it is the Chain
  tier asking the node for the hash of the newest recorded block, to
  compare with the recorded one.
- "no gap: work left" became a symbol of two bars back to back, "back to
  back: work was left", and the legend says why.
- The buttons' misalignment came from the site-wide button margin; the
  timeline's buttons and readout are now the track's height.

## What differs from the design, and what is left

- **Simplified time lens.** The design asked for minimum animation lengths,
  sleeps shortened to a second, and speeding up to 2x or 4x when behind.
  Built: frames play at real time 1.5 s behind, each animation has its own
  length (CSS), sleeps are real (about a second, the poll interval), and a
  page more than 40 frames behind skips to the newest. On a caught-up
  engine nothing more was needed; a long catch-up could still use the
  speed-up.
- **Merging runs.** A catch-up writes one line per block on the timeline and
  in the table. Merging runs of header-only or catch-up blocks into one
  line ("blocks 1,000 to 1,300 recorded") would keep a long catch-up
  readable; the machine is where it would go.
- **Pool dots.** The snapshot lists the first 14 remembered pool
  transactions in id order (the engine's memory keeps no arrival order),
  plus those the page saw match. "The newest few" would need the engine to
  remember when each arrived.
- **Upkeep's detail** shows the rows pruned; the void recheck and scanned
  ranges are drawn as lit squares without counts, as their units report
  none.


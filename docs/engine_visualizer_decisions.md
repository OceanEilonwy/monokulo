# Engine page: implementation decisions

The calls made while building the engine page (`docs/engine_visualizer.md`)
without the reviewer at hand, each with what else was possible and why this.
Newest last.

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

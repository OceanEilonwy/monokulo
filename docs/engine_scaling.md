# Engine scaling: slow links, large blocks, and showing the admin why

Status: built (review finding 30, extended), in four phases: ce43ad1
(measure and adapt), 12cc21c (leaner memory), 9926524 and 56b14b0 (seeing
it), 1359ccc (large blocks). Where the build differs from the proposal
below, "As built" near the end says how and why.

## The problem

The engine fetches blocks from a Monero node in chunks sized by
`payment.scan_chunk_memory_budget_mb`, and every node request has one fixed
15 s timeout (`daemon_rpc::REQUEST_TIMEOUT`) inside a 15 s per-call deadline
(`work::CALL_DEADLINE`).

- **A slow link stalls the scan for good.** A chunk that can't arrive in 15 s
  fails. The chunk size only adapts on success (a running average of bytes
  per block), so the next round asks for the same chunk and fails again. At
  the default 8 MB budget, any link under about 4.5 Mbit/s stalls.
- **A timeout leaves no time to fall back.** The outer deadline equals the
  client timeout, so a primary node that times out leaves no time to try a
  fallback node.
- **Blocks can't grow past one response.** A block is always fetched whole by
  `get_blocks.bin`, so a block larger than the response cap (64 MB) or the
  memory budget (at most 48 MB) can never be scanned. Monero's block size is
  dynamic, so the engine should handle 100-500 MB blocks on hardware that can
  hold them, rather than assume today's averages (about 13 kB a block, pruned).
- **Memory is spent about three times over.** A chunk exists as the raw body,
  as blobs copied out while parsing, and as fully decoded transactions, which
  the block cache then keeps.
- **The admin can't see any of this.** Nothing shows a node's speed, why the
  scan goes at the pace it does, or how close the engine is to its memory
  limit.

What already helps:
- Transactions are fetched pruned (prefix and RingCT base only).
- A block's scan can already stop part-way and resume by transaction index
  (`partial_block_progress`). Matches stay staged until the block's hash is
  rechecked (`docs/scanner_microtasks.md`).
- Rounds have a deadline with a reserved share per tier, so other work keeps
  moving.

## Goals

1. A scan always makes progress on any link that can carry one transaction
   within the timeout ceiling.
2. Fast nodes get big requests; slow nodes get small ones that finish.
3. Memory use stays close to the configured budget, and the budget is
   checked against the memory the engine really has.
4. A very large block is scanned in pieces, and mempool detection, settlement
   and reorg checks keep running between the pieces.
5. When the engine is slow, the admin sees that it's slow, why, and what
   would help.

## 1. Measuring each node

Each node keeps three running averages (EWMA), in memory, next to its
existing health state in `daemon_fallback`:

| Measure | From | Why separate |
|---|---|---|
| Round-trip time (RTT) | small JSON calls (height, tip, info) | the fixed cost of any call |
| Time to first byte (TTFB) per block requested | `get_blocks.bin`: time until response headers, divided by the block count | monerod builds the whole answer before sending, so its own work grows with the request; it isn't a constant |
| Transfer rate (bytes/s) | body bytes ÷ (last byte − first byte), for bodies over 256 kB only | below that, latency dominates and the rate would read low |

- **Cold start:** a new node (or a restart) starts at 1 Mbit/s, 1 s RTT and
  50 ms TTFB per block. These are deliberately pessimistic and correct within
  a few calls.
- **Not persisted:** a stale measurement is worse than a quick re-measure.
- **A timeout counts as evidence:** a timed-out call halves the node's rate
  estimate. Otherwise a node that only ever times out would never get a lower
  estimate.

## 2. Timeouts and chunk sizes from the measurements

For a request expected to return `B` bytes covering `n` blocks:

```
expected = RTT + n × TTFB_per_block + B ÷ rate
timeout  = clamp(3 × expected, 15 s, 10 min)
```

- **Per call:** small JSON calls keep 15 s. The engine's per-call deadline
  (`work::bounded`) gets the same number plus one more attempt's worth, so a
  fallback node still has time.
- **Chunk size** (blocks per `get_blocks.bin`) is the smaller of two limits:

  ```
  by_memory = response_cap ÷ avg_block_bytes
  by_time   = rate × target_call ÷ avg_block_bytes   (target_call ≈ 4 s)
  chunk     = clamp(min(by_memory, by_time), 1, 500)
  ```

  `response_cap` is defined in section 3 (an eighth of the budget).
- **Why the time limit is in seconds.** The scheduler shares out a round's
  seconds, not bytes. Each tier has a share of the 10 s round, and a tier
  always runs at least one unit, which can't stop part-way through a node
  request. One block request is therefore the smallest delay the Blocks
  tier can cause the mempool, settlement and upkeep tiers. `target_call`
  is the Blocks tier's share, 40 % of 10 s, and the code derives it from
  `work::ROUND_BUDGET` and the tier shares
  (`scanner::SCAN_CHUNK_TARGET_CALL_SECS`). A limit in bytes alone would
  be milliseconds on a LAN node and minutes over Tor.
  The limit doesn't cap throughput. A round that ends with blocks left is
  followed at once by the next, so a slow link stays about as busy as it
  would with larger requests, which would only spread the round trip over
  more bytes. On a fast link `by_memory` binds first. At the default 8 MB
  budget, `by_time` takes over below about 2 Mbit/s (1 MB ÷ 4 s).
- **Failure halves, success grows:** a timeout or an over-cap answer halves
  the next chunk (by doubling the bytes-per-block estimate) and halves the
  node's rate. Successes grow both back through the running averages. Any
  link that can deliver one block moves forward; the worst case is one
  block per call.
- **The size used for averages is the wire size** of each block, read from the
  response itself, so nothing is re-serialized just to be measured.

## 3. Memory

### Holding a chunk once, not three times

1. **Decode into the scan's own form.** The block cache keeps, per
   transaction, only what the scan reads: output keys and view tags, the
   transaction public keys from `extra`, the encrypted amounts and
   commitments, and the transaction id. This is `ScanInput` and its parts;
   the full `Transaction` and the raw blob are dropped as soon as each is
   decoded.
2. **Cap each response separately from the cache:**
   `response_cap = budget ÷ 8`. The raw body and parse copies then cost a
   fraction of the budget, not a multiple of it.
3. **Release per block:** a block's raw bytes are dropped once it is decoded,
   not when the whole response is done.

Peak use is then about 1.25 × the budget. A truly streaming decoder (reading
`get_blocks.bin` entries as bytes arrive) would only matter if one response
had to approach the whole budget, and segmenting (section 4) avoids that.

### Checking the budget against the machine

- **Available memory:** the smaller of total RAM and, on Linux, the cgroup
  limit (`/sys/fs/cgroup/memory.max`). In a container, host RAM is the wrong
  number.
- **The rule:** `budget × configured networks × 1.25 ≤ 80 % of available`.
- **Wider range:** the budget setting accepts any value from 1 MB up; the old
  ceiling of 48 MB and the fixed 64 MB response cap go. A value over the
  machine's limit is refused with the real maximum:
  "At most 1,536 MB on this machine (80 % of 7.6 GB, across 2 networks)."
- **Shown on the setting:** the setting's help text shows the same maximum.
- **Rechecked at start:** a database or environment value that no longer
  fits (the machine shrank) makes the engine start with the largest budget
  that fits, and raises an admin alert saying so.

## 4. Very large blocks: segments sized to the round

- **Decide before fetching.** For the next block, ask for its header first
  (`get_block_header_by_height` gives `block_weight` and `num_txes`). Batched
  for a run of small blocks, this costs one cheap call per chunk.
- **Whole mode,** for a block expected to fit one response and one target
  call: fetched with neighbours in a `get_blocks.bin` chunk, as now.
- **Segmented mode,** for a larger block:
  1. Fetch the block itself (header, miner transaction, transaction hashes)
     with `get_block`.
  2. Fetch its transactions in pages through pruned `/get_transactions`.
     Page size comes from the section 2 formula, so one page is about one
     target call and within the response cap.
  3. Each page is one unit in the Blocks tier. It scans the page for every
     store in the group and records `partial_block_progress` with the next
     transaction index.
  4. Once the last page is done, recheck the block's hash and commit the
     cursor with the staged matches, as now.
- **CPU is sized too.** A page also costs one scan per transaction per store.
  The engine keeps a running average of scan time per transaction per store,
  and shrinks the page when the CPU, not the link, is what would overrun the
  slice.
- **The rest of the engine keeps going.** A 500 MB block becomes many units
  across many rounds. Mempool detection, settlement and reorg checks run
  between them, so a giant block delays only block confirmations.

### The round deadline

- **Fixed base:** rounds keep their 10 s deadline and tier shares
  (`work::ROUND_BUDGET`).
- **Grows only when it must:** only if the smallest useful unit (one page
  holding one transaction) is estimated to need more than the Blocks share.
  Then the deadline becomes `max(10 s, 1.5 × that unit's estimate)`, capped at
  120 s.
- **Shown:** the status page and admin panel show that it was raised, and why.

## 5. The slow-block state

- **Trigger:** our own processing time for one block. The clock starts when
  the engine starts fetching a block and stops when that block's scan is
  committed. If it passes **2 minutes** while the node is answering, the
  network is **slow**. The threshold is a constant, not a setting.
- **Not a trigger:** the network's own block rate. Gaps between blocks
  fluctuate (a block can take many minutes to be mined) and say nothing
  about the engine. How far behind the tip we are isn't a trigger either: a
  backlog of small blocks scanned quickly is healthy. Only one block taking
  us too long is.
- **What it shows:**
  - The status indicator goes **yellow** (`status-dot-slow`), a new state
    between healthy (green) and a problem (red). Yellow means only this:
    slow but moving.
  - The status page shows a warning banner for that network.
  - The admin sees the same alert in the dashboard alert bar.
  - It uses the existing `--warning` theme role, in both themes.
- **The message says why and what would help.** For example:

  > Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far, at
  > 3.1 Mbit/s from node.example:18089. At this rate it needs about 18
  > minutes. A faster node or a larger scan memory budget would help.

- **Clears** by itself when the block completes. A node that doesn't answer
  stays red, as now.

## 6. What the admin sees

On the **Monero nodes** tab, next to the settings each number describes.
Everything is rendered on the server (works without JavaScript) and
refreshes in place with fixi. Charts are small inline SVG with a text
summary beside them, and use theme roles only.

### Resources (top of the tab)

One figure each for CPU and memory, for monokulo and the engine together,
with a stacked chart that shows the total and each process's share on the
same scale:

```
CPU      25 % of 4 cores   (engine 23 %, monokulo 2 %)      peak 63 % at 14:02
         ┌──────────────────────────────────────────────┐
   100 % │                                              │
         │            ▄▄                                │   ■ engine
         │       ▂▄▄▆▆██▆▄▂        ▂▂                   │   ■ monokulo
     0 % │▁▁▂▂▃▄▅████████▅▄▃▂▂▂▃▄▅██▅▃▂▂▁▁▁▁▂▂▃▃▂▂▁▁▁▁▁▁│
         └──────────────────────────────────────────────┘
          -60 min                                    now

Memory   508 MB of 7.6 GB  (engine 412 MB, monokulo 96 MB)  peak 551 MB
         (same chart, scaled to the machine's memory)
```

- **Total:** the sum of both processes, against the machine's capacity:
  CPU as a share of all cores, memory against total RAM.
- **Stacked layers:** each process is its own coloured layer, engine at the
  bottom and monokulo on top, so the outline is the total and each band is
  one process's share. A legend names the colours, and the figure line
  repeats the current split in numbers.
- **Colours** are two new chart roles in `theme.css` (`--chart-engine`,
  `--chart-monokulo`), defined for both themes and distinguishable for the
  common colour-vision deficiencies. No colour is written in the markup.
- **Peak** is the total's peak in the last hour, with when it was.
- **Hover detail** (with JavaScript): each 10-second slot shows its total
  and split. Without JavaScript the page shows the current figures and the
  peak, and the chart is still drawn.
- **Container limits:** when either process runs under a cgroup memory
  limit, a thin line marks it on the memory chart, so an admin can see a
  process nearing its own limit before the machine fills.

How it's gathered:
- **Sampling:** each process samples itself every 10 s (process CPU time
  and resident memory, read with the `sysinfo` crate on Linux and macOS,
  plus the cgroup files on Linux), keeping the last hour (360 points) in
  memory. The engine's samples come in its `/status` `scaling` section.
- **Merging:** monokulo merges the two series by 10-second slot. A slot
  missing from either process is drawn as a gap, not a zero.
- **Same machine only:** a total only means something on one machine. Each
  process reports the host's `boot_id` (`/proc/sys/kernel/random/boot_id`,
  which containers on one host share; on macOS the host name and total
  memory). When the two match, they stack as above. When they don't, the
  panel shows the two processes as separate charts and says they run on
  different machines.

### Each node row

- Status, the node's height, and how far it is behind the network.
- Transfer rate, RTT and TTFB, each with a one-hour sparkline.
- When last measured, and failures and timeouts in the last hour.

### One "Scanning" panel per network

```
Mainnet scanning                                          ● slow
  Progress      14 blocks behind · 3.2 blocks/min · caught up in ~4 min
  Pace set by   link speed (node.example:18089, 3.1 Mbit/s)
  Chunk         1 block (segmented: page 41 of 97, 4.2 MB pages)
  Block size    avg 1.8 MB (rising ↗) · largest recent 412 MB, took 18 m
  Memory        budget 256 MB · peak 301 MB · machine allows 1,536 MB
  Round         deadline 10 s (base)
  Alerts        Block 3,412,001 has taken 2 m 10 s so far …
```

- **"Pace set by"** names the one thing limiting the scan right now: the
  memory budget, link speed, round time, CPU (scan cost per store), or
  "caught up". It's the line that tells the admin what to change.
- **Chunk** says how big the current request is and why.

### Where the numbers come from

- **Engine:** a new `scaling` section of the engine's `/status` JSON, with
  per-node measurements, per-network scan figures and the engine's resource
  samples (with its `boot_id` and cgroup limit).
  Everything on the engine already requires the engine token.
- **monokulo:** renders it for admins only. Anonymous visitors to the status
  page see the network state and the slow banner, never node addresses or
  rates.

## As built

Where the code differs from the proposal above, or settles something it
left open:

- **Memory test (phase 2).** No counting-allocator test of a chunk's peak:
  a global allocator in the engine's test binary would count every other
  test running beside it. Structural tests stand in: the response cap
  follows the budget, a block's answer is dropped before its blocks are
  decoded, and the cache keeps only the compact `ScanTx`.
- **A budget too large at start (section 3).** The engine doesn't clamp it.
  As for any setting that fails validation, the scan settings fall back to
  their defaults, and the refusal says the machine's real maximum. The
  admin page shows that maximum on the setting.
- **Paging threshold (section 4).** A block is scanned in pages when its
  weight is over the response cap or would take its node's link over 30
  seconds; a block whose header gives no weight is fetched whole. A page
  holds at most 100 transactions, the most a restricted (public) node
  returns from one `/get_transactions`.
- **Headers first only while blocks may be large (the open question).**
  Blocks are fetched whole, with no headers call, until something says a
  block may not fit: a block request that runs out of time or comes back
  too large, or a fetched block within a quarter of the size that is paged.
  Each such sign turns headers-first on for an hour; a block being paged
  keeps it on. Headers then come 256 at a time. A giant block arriving
  while it is off costs one refused request, bounded by the response cap
  or the link's timeout, and the next try pages it. The Scanning panel
  says whether it is on, and why.
- **The block cache outlasts the round (round length sweep).** A run
  fetched ahead used to be dropped when its round ended and fetched again
  in the next, so a link-limited catch-up re-sent up to one chunk a round
  (12 to 19 times the distinct blocks at 2 s rounds, 1.3 to 1.4 at 10 s:
  `cargo xtask stress rounds`). A round now starts from the cache the last
  one left and leaves what may serve the next: blocks above every tenant's
  cursor and at least `reorg_check_depth` below the tip, from the same node
  (`MoneroDaemonClient::node`), never after a rewind or while a reorg job is
  open. The cache stays within the scan memory budget at all times, and is
  trimmed to it again at each round's start in case the setting was
  lowered. A caught-up network holds nothing. Catch-up groups share the
  budget: each group's request is capped at its share (the budget divided
  by the groups and the frontier, within the response cap), so every
  group's run fetched ahead fits at once, and blocks already scanned are
  evicted before runs fetched ahead. Without the share, 16 groups at the
  default budget evicted each other's runs: 4.7 blocks a second and 9 MB
  discarded, against 13.4 and 130 kB with it. Blocks let go of unread are counted
  (`discarded_cache_bytes` in `/status`). The peak is unchanged, since one
  round could already fill the budget; only how long it is held changed, so
  the `budget × networks × 1.25` check stands.
- **Checkpoints.** A page doesn't write `partial_block_progress` by itself.
  The existing checkpoint is written when a unit runs out of time or a
  page fails, which is when a resume needs it. A crash costs at most the
  pages since the last one, as for any unit.
- **The round's time (section 4).** It is computed from the large block in
  progress, if any, each round: one page of one transaction (round trip,
  first byte, its bytes at the link's rate, its scan for every store).
  `/status` reports it as `round_deadline_secs`.
- **Hover detail (section 6).** Each minute of a resource chart is an SVG
  `<title>`, which browsers show on hover with or without JavaScript. A
  Refresh link (fixi) reloads the tab; nothing refreshes on its own.
- **Chart colours.** Okabe-Ito blue (engine) and vermillion (monokulo),
  lighter in dark mode, each tested at 3:1 against the card.
- **Node addresses.** The slow-block sentence names the node and its rate
  for operators only. The status page shows anyone else the rate alone.
- **Not built:** the 200 MB test uses a block whose header says 200 MB
  rather than 200 MB of transactions. The paging decision, page sizes and
  the scan are the same either way, and the test stays fast.

## Phases

| Phase | Delivers | Tested by |
|---|---|---|
| 1. Measure and adapt | per-node RTT/TTFB/rate; adaptive per-call timeout and deadline; chunk from memory and time; halving on failure | a fake node with a throttled body and a slow first byte: the scan completes at a low rate; a timeout halves the next chunk; the timeout follows the measured rate; a fallback node gets time after a primary times out |
| 2. Leaner memory | compact decoded form; response cap; per-block release; budget check against RAM and the cgroup limit; fixed caps removed | peak allocation for a chunk stays within 1.3 × budget (counting allocator in a test); budget validation against a fixed memory figure; the start-up clamp and its alert |
| 3. Seeing it | resource sampling in both processes and the stacked chart; node rows; Scanning panel; slow (yellow) state and banner | view tests for each panel state (light and dark gallery screenshots, stacked and different-machine charts); merging by slot with gaps; the slow state starts after 2 minutes on a paused clock and clears when the block completes |
| 4. Large blocks | header-first mode choice; segmented fetch through `/get_transactions` pages; CPU-aware page size; round deadline floor | a fake node serving a 200 MB block: scanned in pages with bounded memory; a payment in the block's last page is found and staged until the hash recheck; mempool detection keeps working mid-block |

Phase 1 closes review finding 30. Each phase is its own commit series and
leaves the engine working.

## Open questions

- monerod's `get_blocks.bin` appears to cap one answer at about 100 MB but
  always return at least one block. This needs verifying against monerod's
  source; segmented mode avoids depending on it either way.

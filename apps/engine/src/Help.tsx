// The legend behind the header's (?): what every label, symbol and movement
// on the page means.
import { HelpTip } from '@monokulo/ui/solid';

const tier = (name: string) => <i class={`tierchip t-${name}`} aria-hidden="true" />;
const swatch = { width: '11px', height: '13px' };

export function Help() {
  return (
    <HelpTip label="What the page shows" id="engine-help" wide={true} anchor="container">
      <div class="engine-legend">
        <p class="wide">The engine scans the Monero chain for the stores' payments in rounds. This page follows one network's scanner about 1.5s behind, and the timeline replays the last 30 minutes.</p>

        <h3>Timeline</h3>
        <dl>
          <dt><svg width="10" height="16" aria-hidden="true"><line x1="5" y1="2" x2="5" y2="14" stroke="var(--muted)" stroke-width="1.5" /></svg></dt>
          <dd>An event.</dd>
          <dt><svg width="14" height="14" aria-hidden="true"><circle cx="7" cy="7" r="5" fill="var(--viz-tier-blocks)" /></svg></dt>
          <dd><b>A key event</b>, in its tier's colour: a payment found, a reorganisation, a block's scan saved partway, stores catching up. Hover over it for what it was.</dd>
          <dt><svg width="10" height="16" aria-hidden="true"><rect x="4" y="1" width="2" height="14" fill="var(--ink)" /></svg></dt>
          <dd><b>The playback position</b>: the moment the whole page is showing, 1.5s behind the engine while live. Drag it to scrub (that pauses there, as Pause does), press anywhere in the window to go there; Play replays from it, Live returns.</dd>
        </dl>
        <dl>
          <dt><span class="tl-win" style={{ position: 'static', display: 'block', width: '22px', height: '12px' }} /></dt>
          <dd><b>The window</b>: the bar is always the last 30 minutes, the history filling it from the right; paused, it stops at the moment you left live. The orange window is the stretch you are looking at: drag its middle to move it, its handles to widen or narrow it. While it ends at now the page is live; move it into the past and playback pauses at its start.</dd>
          <dt><span class="hatch" aria-hidden="true" /></dt>
          <dd><b>Hatched</b>: the engine has no record of that time, because it started (or restarted) since. There is nothing to show or move the window to there; the faint line is where its record starts.</dd>
          <dt><kbd>←</kbd></dt>
          <dd><b>Live, Play, Pause</b> choose how the page plays: following the engine, replaying from the playback position, or held still; off live, it says how far behind live it is. With the window focused, left and right jump between key events (with Shift, any event), space plays and pauses, End goes live.</dd>
        </dl>

        <h3>Summary</h3>
        <p class="wide"><b>Node tip</b>: the newest block the node has. <b>Scanned to</b>: the high-water mark, the newest block the engine has recorded. <b>Behind</b>: blocks between the node's tip and the store furthest behind. <b>Last round</b>: how long the last round took, of its 10s budget. <b>Chain</b>: whether the recorded chain still agrees with the node's.</p>

        <h3>Machine and links</h3>
        <p class="wide"><b>CPU</b> and <b>Memory</b>: the machine's last hour, the engine's share under monokulo's; a dashed line is a container's memory limit. <b>Transfer</b>, <b>Round trip</b> and <b>First byte</b>: the link to the node the engine is reading from right now, named under the figure (a fallback says so). <b>Block size</b>: the average of recent blocks. They are updated every few seconds; the chip says how long ago.</p>

        <h3>Chain</h3>
        <dl>
          <dt><i class="cell" aria-hidden="true" style={swatch} /></dt>
          <dd>A block the engine has recorded.</dd>
          <dt><i class="cell new" aria-hidden="true" style={swatch} /></dt>
          <dd>A block the node has that the engine hasn't scanned yet.</dd>
          <dt><i class="cell cached" aria-hidden="true" style={swatch} /></dt>
          <dd>Fetched from the node and held in memory to be scanned (the cache chip says how much).</dd>
          <dt><i class="cell reorg" aria-hidden="true" style={swatch} /></dt>
          <dd>Replaced: the node's chain no longer has this block (a reorganisation).</dd>
          <dt><span class="cell" aria-hidden="true" style={{ ...swatch, display: 'block' }}><span class="fill" style={{ width: '60%' }} /></span></dt>
          <dd>A block being scanned fills left to right. A dark corner square means its scan was saved partway, to carry on next round.</dd>
        </dl>
        <dl>
          <dt><i class="cell ghost next" aria-hidden="true" style={{ width: '13px', height: '13px' }}><span class="pool" style={{ height: '55%' }} /></i></dt>
          <dd><b>The next block</b>, not mined yet. It fills from the bottom with the transactions waiting in the node's pool (the number) against what a miner can fit in a block at full reward. The red top edge is where the penalty zone starts, and it thickens when the pool holds more than a block's worth. The page asks the node every 5s while it is open.</dd>
          <dt><span class="engine-chip" style={{ 'font-size': '0.6rem', padding: '0 4px' }}>tip</span></dt>
          <dd><b>node tip</b> and <b>scanned to</b> mark the node's newest block and the engine's high-water mark. The blue line under the last blocks is the reorg window, checked again for a reorganisation every round.</dd>
          <dt><span class="pill frontier" style={{ position: 'static', transform: 'none', padding: '0 5px', 'font-size': '0.6rem' }}>F</span></dt>
          <dd><b>Groups of stores</b>, by the block their scan has reached. <b>Frontier</b> stores are at the high-water mark and scanned as each block arrives. <b>Catching up</b> stores are behind (a new store, or after downtime) and scanned with time left over until they join the frontier.</dd>
        </dl>

        <h3>Round</h3>
        <p class="wide">Each round gives the five tiers a share of a 10s budget, in order; time left over goes round again. Every millisecond of a round belongs to one tier, so the times written after the segments add up to the round's: the round's opening (deciding whether to ask for the pool, then asking the node for its tip) counts to Chain, keeping fetched blocks for the next round to Blocks. A tier's work that ran back to back is one segment; hover for its total duration and operation count, or open Timing details for the breakdown.</p>
        <dl>
          <dt>{tier('chain')}</dt>
          <dd><b>Chain</b> asks the node for its tip, then checks the recorded chain still matches the node's: it compares the newest recorded block's hash with the node's. When the engine is caught up, the tip's hash came with the tip and nothing more is asked; otherwise a blue <b>hash check</b> flies from the node. If they differ, it reconciles the reorganisation: payments are re-examined and blocks rewound.</dd>
          <dt>{tier('blocks')}</dt>
          <dd><b>Blocks</b> fetches new blocks from the node and scans each one for every group of stores.</dd>
          <dt>{tier('mempool')}</dt>
          <dd><b>Mempool</b> scans transactions still in the pool, so a payment is seen before it is mined. It runs only while an order waits to be paid.</dd>
          <dt>{tier('settlement')}</dt>
          <dd><b>Settlement</b> turns what the chain and the pool say into each order's status (paid, confirming, expired) and writes the order event Monokulo sends the store's webhooks from.</dd>
          <dt>{tier('upkeep')}</dt>
          <dd><b>Upkeep</b> does a little housekeeping each round: pruning old block hashes, rechecking voided payments.</dd>
        </dl>
        <dl>
          <dt><small>40 %</small></dt>
          <dd>The tier's share of the round's budget.</dd>
          <dt><span class="bar sample t-blocks"><i class="fill" style={{ left: '0', width: '100%' }} /></span></dt>
          <dd>A unit of work. Striped <span class="bar sample t-blocks"><i class="fill p2" style={{ left: '0', width: '100%' }} /></span> ran on time left over (pass 2). An outline <span class="bar sample t-chain"><i class="work" style={{ left: '0', width: '100%' }} /></span> is work for the tier outside its units, such as the tip request, drawn over only the time it took. When other tiers ran between a unit and that later work, a thin thread crosses the time between <span class="bar sample t-blocks" style={{ width: '40px' }}><i class="fill" style={{ left: '0', width: '40%' }} /><i class="thread" style={{ left: '40%', width: '45%' }} /><i class="work" style={{ left: '85%', width: '15%' }} /></span>; when that time is short, the outline starts right where the unit ends instead. Hover over a bar for what it was. While stores catch up or a reorganisation is open, a dashed box shows the tier's reserved share.</dd>
          <dt><span class="ruler-label" style={{ position: 'static', transform: 'none' }}>s</span></dt>
          <dd>Each segment's time is written after it. The thin marker is on the right edge of the segment that finished last, and the label under it is the round's time: the sum of the segments'.</dd>
          <dt><span class="engine-chip ok" style={{ 'font-size': '0.6rem' }}>Idle</span></dt>
          <dd><b>Idle</b>: nothing left to do. <b>Backlogged</b>: out of time with work left, so the next round starts at once. <b>Waiting</b>: held up by what it names. <b>Failed</b>: an error, retried next round.</dd>
        </dl>

        <p class="wide">Round numbers count scanner rounds for this network since the engine started. They reset to 1 when the engine restarts; refreshing this page or reconnecting the live stream does not reset them.</p>
        <h3>Last rounds</h3>
        <dl>
          <dt><span class="rbar" style={{ height: '14px', width: '8px' }}><i class="t-blocks" style={{ height: '60%' }} /><i class="t-chain" style={{ height: '40%' }} /></span></dt>
          <dd>A round. The taller, the longer (on a log scale), coloured by where its time went. Click one to show it in the round card; its <b>× Paused</b> chip goes back to the live round.</dd>
          <dt><i class="rgap" aria-hidden="true" /></dt>
          <dd>The scanner slept until the poll interval was up.</dd>
        </dl>
        <dl>
          <dt><i class="rgap woken" aria-hidden="true" /></dt>
          <dd>The sleep was cut short: the node announced a new block.</dd>
          <dt><i class="rpair" aria-hidden="true"><i /><i /></i></dt>
          <dd>Rounds back to back with no sleep between: the first ended with work left, so the next started at once.</dd>
        </dl>

        <h3>Things that move</h3>
        <dl>
          <dt><span class="token pkt chain" style={{ position: 'static', display: 'inline-block' }}>hash</span></dt>
          <dd>A call to the node, flying from the node to where its answer goes: a hash check (blue), blocks (orange), the pool or transactions (green).</dd>
          <dt><span class="token token-stores" style={{ position: 'static', display: 'inline-block' }} /></dt>
          <dd>Stores moving on to the next block.</dd>
        </dl>
        <dl>
          <dt><span class="token payment" style={{ position: 'static', display: 'inline-block' }} /></dt>
          <dd>A payment found, on its way to the order's status.</dd>
          <dt><span class="saved-glyph" aria-hidden="true" /></dt>
          <dd>Something saved to disk. A restart carries on from there.</dd>
        </dl>
        <p class="wide">The panels under the round sum up the scan's pace, the reorg check, the pool, orders, upkeep, the database worker and what a restart would lose. Click one for its figures.</p>
      </div>
    </HelpTip>
  );
}

// The engine page's script (docs/engine_visualizer.md). Without it the page
// is a point-in-time view rendered by monokulo; with it, the page follows the
// network live.
//
// It draws, and nothing else: every word and figure comes from the server
// (`engine_view::present`), every state from its state machine
// (`engine_view::machine`). The server sends frames (what the page shows at a
// moment, and the effects that led there); this script plays them about 1.5 s
// behind the engine, animates the effects, and runs the timeline: one bar
// of the whole history, with a window over it to move and resize, and
// inside the window, once off live, the playback position; each moment is
// asked of the server (`/status/engine/at`, `/status/engine/replay`).
//
// The page never zooms on the mouse wheel: scrolling scrolls the page.
(() => {
  "use strict";
  const page = document.querySelector("main.engine-page[data-network]");
  if (!page || !window.EventSource) return;
  const network = page.dataset.network;
  if (!network) return;
  const $ = (id) => document.getElementById(id);
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const LAG = 1500;
  const MIN_SPAN = 5000;
  const REPLAY_MS = 10000;
  const CELL_PX = 26;
  const TIERS = ["chain", "blocks", "mempool", "settlement", "upkeep"];
  const TIER_NAMES = { chain: "Chain", blocks: "Blocks", mempool: "Mempool", settlement: "Settlement", upkeep: "Upkeep" };

  const esc = (text) => String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const fmt = (n) => Number(n).toLocaleString("en-GB");

  // ---- state ----
  let offset = 0; // engine clock minus ours
  let oldest = null; // the oldest moment the history has
  let marks = []; // every mark, oldest first
  let queue = []; // frames waiting to be played, oldest first
  let view = null; // what is drawn now
  let head = 0; // the moment drawn, engine milliseconds
  let mode = "live"; // live | paused | replay
  let replayTo = 0;
  let replayFetching = false;
  let source = null;
  const engineNow = () => Date.now() + offset;

  // ---- the stream ----
  function connect() {
    if (source) source.close();
    source = new EventSource(`/status/engine/events?network=${encodeURIComponent(network)}`);
    source.addEventListener("history", (e) => {
      const start = JSON.parse(e.data);
      offset = start.engine_now_ms - Date.now();
      oldest = start.oldest_ms;
      marks = start.marks;
      queue = [];
      head = start.frame.at_ms;
      draw(start.frame.view);
      drawEvents();
      tlDirty = true;
    });
    source.addEventListener("frame", (e) => {
      const frame = JSON.parse(e.data);
      if (oldest === null) oldest = frame.at_ms;
      const arrived = performance.now();
      for (const mark of frame.marks) { mark.arrived = arrived; marks.push(mark); }
      if (mode === "live") queue.push(frame);
      tlDirty = true;
    });
    source.addEventListener("restarted", () => connect());
    source.addEventListener("unreachable", () => setReadout("The engine isn't answering; showing what was last seen"));
  }

  // ---- playback ----
  let lastTick = performance.now();
  function frameLoop(tick = performance.now()) {
    const dt = Math.min(250, tick - lastTick);
    lastTick = tick;
    if (mode === "live") {
      head = Math.max(head, engineNow() - LAG);
      while (queue.length && queue[0].at_ms <= head) play(queue.shift());
      // Far behind (a hidden tab): skip to the newest.
      if (queue.length > 40) { const last = queue.pop(); queue = []; play(last, true); }
    } else if (mode === "replay") {
      head += dt;
      while (queue.length && queue[0].at_ms <= head) play(queue.shift());
      if (!queue.length && head >= replayTo) fetchReplay();
      // Replay plays the window: it pauses at the window's end, or goes
      // live when the window ends at now.
      const [, b] = view_();
      if (win.end != null && head >= b) { head = b; setMode("paused"); }
      else if (head >= engineNow() - LAG) goLive();
    }
    if (tlDirty || mode !== "paused") drawTimeline();
    requestAnimationFrame(frameLoop);
  }

  function play(frame, quietly) {
    if (!quietly && !document.hidden && !reduced) {
      const first = frame.effects.length ? frame.effects[0].at_ms : 0;
      for (const timed of frame.effects) {
        const delay = Math.min(400, Math.max(0, timed.at_ms - first));
        setTimeout(() => animate(timed.effect), delay);
      }
    }
    draw(frame.view);
    if (frame.marks.length) drawEvents();
  }

  // One request at a time: while one is out, a scrub only notes where it
  // got to, and that is asked for next. What arrives is drawn unless the
  // page went live meanwhile.
  let seeking = false, seekWanted = null;
  async function seek(t) {
    t = Math.max(oldest ?? t, Math.min(t, engineNow() - LAG));
    head = t;
    queue = [];
    tlDirty = true;
    seekWanted = t;
    if (seeking) return;
    seeking = true;
    try {
      while (seekWanted !== null) {
        const wanted = seekWanted;
        seekWanted = null;
        const response = await fetch(`/status/engine/at?network=${encodeURIComponent(network)}&ms=${Math.round(wanted)}`);
        if (!response.ok) continue;
        const frame = await response.json();
        if (mode === "live") break;
        clearTokens();
        draw(frame.view);
        drawEvents();
      }
    } finally {
      seeking = false;
    }
  }

  async function fetchReplay() {
    if (replayFetching) return;
    replayFetching = true;
    const from = Math.round(head), to = from + REPLAY_MS;
    try {
      const response = await fetch(`/status/engine/replay?network=${encodeURIComponent(network)}&from=${from}&to=${to}`);
      if (response.ok && mode === "replay") {
        queue = (await response.json()).filter((f) => f.at_ms > head);
        replayTo = to;
      }
    } finally {
      replayFetching = false;
    }
  }

  // The playback radio group follows the mode: Play only off live. Leaving
  // live freezes the timeline's axis where it was; going live lets it go.
  function setMode(next) {
    if (mode === "live" && next !== "live") frozenEnd = engineNow();
    if (next === "live") frozenEnd = null;
    mode = next;
    for (const radio of document.querySelectorAll('#tl-modes input[name="tl-mode"]')) {
      radio.checked = radio.value === mode;
      if (radio.value === "replay") radio.disabled = mode === "live";
    }
    tlDirty = true;
  }

  function goLive() {
    setMode("live");
    setWindow(null, win.span);
    connect();
  }

  // ---- drawing a view ----
  function draw(next) {
    view = next;
    drawSummary(next);
    drawChain(next.chain);
    drawRound(next);
    drawSide(next.side);
  }

  function drawSummary(v) {
    const box = $("engine-summary");
    if (!box) return;
    box.innerHTML = v.summary.map((f) => `<div><span class="k">${esc(f.label)}</span><span class="v">${esc(f.value)}</span><span class="s">${esc(f.note)}</span></div>`).join("");
  }

  const cellEls = new Map();
  const pillEls = new Map();

  function visibleBlocks(chain, cells) {
    if (chain.tip == null || chain.high_water == null) return [];
    const right = Math.max(chain.tip, chain.high_water) + 1;
    const lowest = Math.min(chain.lowest ?? chain.high_water, chain.high_water);
    const left = Math.max(0, Math.min(lowest - 1, right - cells + 1));
    const all = [];
    if (right - left < cells) { for (let h = left; h <= right; h++) all.push(h); return all; }
    for (let h = lowest - 1; h <= lowest + 5; h++) all.push(h);
    all.push(null);
    for (let h = right - (cells - 11); h <= right; h++) all.push(h);
    return all;
  }

  function drawChain(chain) {
    const box = $("cells");
    if (!box) return;
    const width = $("strip-scroll").clientWidth || 800;
    const blocks = visibleBlocks(chain, Math.max(12, Math.floor((width - 30) / CELL_PX)));
    const keep = new Set(blocks.map((h) => (h === null ? "brk" : String(h))));
    for (const [key, el] of cellEls) if (!keep.has(key)) { el.remove(); cellEls.delete(key); }
    for (const el of [...box.children]) if (!el.dataset.key) el.remove();
    const cached = new Set(chain.cached), replaced = new Set(chain.replaced);
    const saved = new Map(chain.checkpoints);
    let previous = null;
    for (const h of blocks) {
      const key = h === null ? "brk" : String(h);
      let el = cellEls.get(key);
      if (!el) {
        el = document.createElement("div");
        el.dataset.key = key;
        if (h === null) { el.className = "brk"; el.textContent = "…"; }
        else { el.innerHTML = '<span class="fill"></span>'; el.dataset.h = key; }
        cellEls.set(key, el);
      }
      if ((previous ? previous.nextSibling : box.firstChild) !== el) box.insertBefore(el, previous ? previous.nextSibling : box.firstChild);
      previous = el;
      if (h === null) continue;
      const state = h > chain.tip ? " ghost" : replaced.has(h) ? " reorg" : cached.has(h) ? " cached" : h > chain.high_water ? " new" : "";
      const next = chain.next_block && h === chain.tip + 1 ? chain.next_block : null;
      const busy = ["enter", "flash", "probe"].filter((c) => el.classList.contains(c)).map((c) => " " + c).join("");
      el.className = "cell" + state + (next ? (next.over ? " next over" : " next") : "") + busy;
      const fill = chain.scanning && chain.scanning[0] === h ? chain.scanning[1] : saved.get(h) || 0;
      el.firstChild.style.width = `${(fill * 100).toFixed(0)}%`;
      const hasSave = saved.has(h), save = el.querySelector(".save");
      if (hasSave && !save) el.insertAdjacentHTML("beforeend", '<span class="save"></span>');
      if (!hasSave && save) save.remove();
      let pool = el.querySelector(".pool");
      if (next && !pool) {
        el.insertAdjacentHTML("beforeend", '<span class="pool"></span><span class="cnt"></span>');
        pool = el.querySelector(".pool");
      }
      if (!next && pool) { pool.remove(); el.querySelector(".cnt").remove(); }
      if (next) {
        pool.style.height = `${((next.fill ?? 0) * 100).toFixed(0)}%`;
        el.querySelector(".cnt").textContent = next.count;
      }
      el.title = next ? next.title : `Block ${fmt(h)}`;
    }
    const x = (h) => {
      const el = cellEls.get(String(h));
      if (el && el.isConnected) return el.offsetLeft + el.offsetWidth / 2;
      const cut = cellEls.get("brk");
      return cut && cut.isConnected ? cut.offsetLeft + cut.offsetWidth / 2 : 0;
    };
    const tip = chain.tip ?? 0, hw = chain.high_water ?? 0;
    let marksHtml = chain.tip != null ? `<span class="m-tip" style="left:${x(tip)}px">node tip</span>` : "";
    if (hw < tip) marksHtml += `<span class="m-hw" style="left:${x(hw)}px">scanned to</span>`;
    if (chain.window_from != null) {
      const from = Math.max(chain.window_from, blocks.find((h) => h !== null) ?? chain.window_from);
      marksHtml += `<span class="m-win" title="The last blocks checked again for a reorganisation every round" style="left:${x(from) - 11}px;width:${x(hw) - x(from) + 22}px"></span>`;
    }
    $("chain-marks").innerHTML = marksHtml;
    $("chain-axis").innerHTML = blocks.filter((h) => h !== null && h % 5 === 0 && h <= tip).map((h) => `<span style="left:${x(h)}px">${fmt(h)}</span>`).join("");
    const pills = $("pills");
    for (const el of [...pills.children]) if (!el.dataset.id || !pillEls.has(el.dataset.id)) el.remove();
    const seen = new Set();
    for (const group of chain.groups) {
      const id = String(group.id);
      seen.add(id);
      let el = pillEls.get(id);
      if (!el) { el = document.createElement("div"); el.dataset.id = id; pills.appendChild(el); pillEls.set(id, el); }
      el.className = `pill ${group.frontier ? "frontier" : "catchup"}${group.busy ? " busy" : ""}${group.waiting ? " waiting" : ""}`;
      el.textContent = group.label;
      el.title = group.title;
      el.style.left = `${x(group.cursor)}px`;
    }
    for (const [id, el] of pillEls) if (!seen.has(id)) { el.remove(); pillEls.delete(id); }
    $("cache-chip").textContent = chain.cache;
    $("nodes").innerHTML = chain.nodes.map((n) => `<div class="node"><div class="nm"><span class="label" title="${esc(n.label)}">${esc(n.label)}</span><span class="engine-chip ${n.tone}">${esc(n.chip)}</span></div></div>`).join("") +
      `<div class="node" id="node-call"><div class="call">${esc(chain.call)}</div></div>`;
  }

  const pct = (ms, scale) => Math.min(100, (ms / Math.max(1, scale)) * 100);

  // A past round chosen from the recent rounds, shown in place of the live
  // one until its "× Paused" chip is pressed.
  let pinned = null, pinnedRibbon = [];
  const live = `/status/engine?network=${encodeURIComponent(network)}`;
  function drawRound(v) {
    const box = $("engine-round");
    if (!box) return;
    let html = "";
    const round = pinned || v.round;
    if (round) {
      const chip = pinned ? `<a class="engine-chip round-paused" id="round-resume" href="${live}" title="Showing a past round: back to the live one">× Paused</a>` : "";
      html += `<header class="round-head"><h2 id="h-round">${esc(round.title)}</h2>${chip}<span class="engine-hint round-state">${esc(round.state)}</span></header><div class="lanes">`;
      for (const lane of round.lanes) {
        html += `<div class="lane-label"><span class="tierchip t-${lane.tier}">${esc(lane.name)}</span><small>${esc(lane.share)}</small></div><div class="track t-${lane.tier}">`;
        if (lane.reserved) html += `<div class="share" style="left:${pct(lane.reserved[0], round.scale_ms)}%;width:${pct(lane.reserved[1], round.scale_ms)}%"></div>`;
        for (const bar of lane.bars) {
          html += `<div class="bar${bar.work ? " work" : bar.leftover ? " p2" : ""}${bar.last ? " last" : ""}" title="${esc(bar.title)}" style="left:${pct(bar.start_ms, round.scale_ms)}%;width:${Math.max(0.5, pct(bar.ms, round.scale_ms))}%"></div>`;
          if (bar.label) {
            // After the bar as drawn: a short one is drawn wider than its time.
            const end = pct(bar.start_ms, round.scale_ms) + Math.max(0.5, pct(bar.ms, round.scale_ms));
            html += `<span class="lane-time${end > 88 ? " before" : ""}" style="left:${Math.min(99.5, end).toFixed(2)}%">${esc(bar.label)}</span>`;
          }
        }
        html += `</div><div class="outcome">`;
        if (lane.outcome) html += `<span class="engine-chip ${lane.outcome.tone}" title="${esc(lane.outcome.text)}">${esc(lane.outcome.text)}</span>`;
        html += "</div>";
      }
      html += `<div></div><div class="ruler"><span class="ruler-label" style="left:${Math.min(99.5, pct(round.elapsed_ms, round.scale_ms))}%">${esc(round.elapsed)}</span></div><div></div></div>`;
    } else {
      html += '<header><h2 id="h-round">Round</h2><span class="engine-hint">No round recorded yet.</span></header>';
    }
    html += '<div class="ribbon-row"><span class="engine-hint">Last rounds</span><div class="ribbon" id="ribbon" aria-label="Recent rounds">';
    for (const mark of pinned ? pinnedRibbon : v.ribbon) {
      if (mark.kind === "round") {
        html += `<a class="rbar${pinned && pinned.number === mark.number ? " pinned" : ""}" href="${live}&round=${mark.number}" data-round="${mark.number}" style="height:${mark.height}px" title="${esc(mark.title)}">${mark.parts.map(([tier, part]) => `<i class="t-${tier}" style="height:${(part * 100).toFixed(1)}%"></i>`).join("")}</a>`;
      } else {
        html += `<i class="rgap${mark.woken ? " woken" : ""}" title="${esc(mark.title)}"></i>`;
      }
    }
    html += '</div><div class="legend"><span><i class="rgap" aria-hidden="true"></i>slept</span><span><i class="rgap woken" aria-hidden="true"></i>woken by a new block</span><span><i class="rpair" aria-hidden="true"><i></i><i></i></i>back to back: work was left</span></div></div>';
    box.innerHTML = html;
  }

  function panel(id, p, extra) {
    const details = $(id);
    if (!details) return;
    const sum = details.querySelector(".sum > span:last-child");
    if (sum) sum.textContent = p.summary;
    const kv = details.querySelector(".kv");
    if (kv) kv.innerHTML = p.rows.map(([label, value]) => `<span>${esc(label)}</span><b>${esc(value)}</b>`).join("");
    if (extra) extra(details);
  }

  function drawSide(side) {
    panel("d-reorg", side.reorg, (d) => {
      d.classList.toggle("alert", side.reorg.alert);
      if (side.reorg.alert && !d.dataset.auto) { d.open = true; d.dataset.auto = "1"; }
      if (!side.reorg.alert && d.dataset.auto) { d.open = false; delete d.dataset.auto; }
    });
    panel("d-mempool", side.mempool, () => {
      $("pool-dots").innerHTML = side.pool_txs.map(([txid, matched]) => `<span class="dot${matched ? " match" : ""}" title="Transaction ${esc(txid)}"></span>`).join("");
    });
    panel("d-orders", side.orders, (d) => {
      const last = side.transitions[0];
      $("orders-sum").innerHTML = `<span>${esc(side.orders.summary)}</span>` + (last ? `<span class="engine-hint">last</span><span class="engine-state state-${last[1]}">${esc(last[1])}</span>` : "");
      const body = d.querySelector(".body");
      body.querySelectorAll(".transition").forEach((el) => el.remove());
      body.insertAdjacentHTML("beforeend", side.transitions.map(([from, to]) => `<div class="transition"><span class="engine-state state-${from}">${esc(from)}</span> to <span class="engine-state state-${to}">${esc(to)}</span></div>`).join(""));
    });
    panel("d-upkeep", side.upkeep);
    panel("d-database", side.database, (d) => {
      d.querySelectorAll(".minibars i").forEach((bar, i) => { bar.style.height = `${2 + Math.min(2, side.queues[i] || 0) * 6}px`; });
    });
    panel("d-webhooks", side.webhooks, () => {
      const most = Math.max(3, ...side.sent);
      const points = side.sent.map((n, i) => `${1 + i * 3},${(16 - (n / most) * 14).toFixed(1)}`).join(" ");
      const svg = $("hooks-sum").querySelector("svg");
      if (svg) svg.innerHTML = `<line x1="1" y1="16.5" x2="89" y2="16.5" stroke="var(--line)" stroke-width="1"></line>` + (points ? `<polyline points="${points}" stroke="var(--ink)" stroke-width="1.5" fill="none" stroke-linejoin="round"></polyline>` : "");
    });
    panel("d-restart", side.restart);
  }

  // ---- events table ----
  const filters = new Set(TIERS);
  function drawEvents() {
    const body = $("engine-events");
    if (!body) return;
    const rows = [];
    for (let i = marks.length - 1; i >= 0 && rows.length < 60; i--) {
      const mark = marks[i];
      if (mark.at_ms > head) continue;
      if (mark.tier && !filters.has(mark.tier)) continue;
      rows.push(mark);
    }
    body.innerHTML = rows.map((m, i) => `<tr class="${m.key ? "key" : ""}${i === 0 && mode !== "live" ? " now" : ""}" data-at="${m.at_ms}"><td class="num">${fmt(m.round)}</td><td>${m.tier ? `<span class="tierchip t-${m.tier}">${TIER_NAMES[m.tier]}</span>` : ""}</td><td>${esc(m.text)}</td></tr>`).join("") ||
      '<tr><td colspan="3" class="engine-hint">Nothing recorded yet.</td></tr>';
  }

  // ---- animations: effects, never state ----
  const stage = $("engine-stage");
  function anchor(a) {
    if (!a) return null;
    switch (a.kind) {
      case "node": return document.querySelector("#nodes .node");
      case "cell": return cellEls.get(String(a.id)) || cellEls.get("brk");
      case "group": return pillEls.get(String(a.id));
      case "pool": return $("pool-dots");
      case "reorg": return document.querySelector("#d-reorg summary");
      case "orders": return $("orders-sum");
      case "webhooks": return $("hooks-sum");
      case "database": return $("db-sum");
      case "upkeep": return $("upd");
      default: return null;
    }
  }
  function centre(el) {
    const s = stage.getBoundingClientRect(), r = el.getBoundingClientRect();
    return [r.left + r.width / 2 - s.left, r.top + r.height / 2 - s.top];
  }
  function fly(from, to, cls, label, ms = 650) {
    if (!from || !to) return;
    const [x0, y0] = centre(from), [x1, y1] = centre(to);
    const token = document.createElement("div");
    token.className = "token " + cls;
    if (label) token.textContent = label;
    token.style.left = `${x0}px`;
    token.style.top = `${y0}px`;
    stage.appendChild(token);
    const dx = x1 - x0, dy = y1 - y0, lift = Math.min(70, 16 + Math.abs(dx) / 10);
    const animation = token.animate([
      { transform: "translate(-50%,-50%)", opacity: 0 },
      { transform: `translate(-50%,-50%) translate(${dx * 0.5}px,${dy * 0.5 - lift}px)`, opacity: 1, offset: 0.5 },
      { transform: `translate(-50%,-50%) translate(${dx}px,${dy}px)`, opacity: 1 },
    ], { duration: ms, easing: "cubic-bezier(.45,0,.35,1)" });
    animation.finished.then(() => token.remove(), () => token.remove());
  }
  function pulse(el, cls) {
    if (!el) return;
    el.classList.remove(cls);
    void el.offsetWidth;
    el.classList.add(cls);
    setTimeout(() => el.classList.remove(cls), 700);
  }
  function clearTokens() {
    stage.querySelectorAll(".token, .ghostcell").forEach((el) => el.remove());
  }
  function callLabel(call) {
    switch (call.kind) {
      case "block_hash": return "hash check";
      case "blocks": return call.count === 1 ? "1 block" : `${call.count} blocks`;
      case "pool": return "pool";
      default: return "txs";
    }
  }
  function animate(effect) {
    switch (effect.kind) {
      case "packet": {
        const cls = effect.call.kind === "block_hash" || effect.call.kind === "transactions" ? "pkt chain" : effect.call.kind === "pool" ? "pkt mempool" : "pkt";
        fly(anchor({ kind: "node" }), anchor(effect.to), cls, callLabel(effect.call), 520);
        break;
      }
      case "fly":
        fly(anchor(effect.from), anchor(effect.to), effect.token === "stores" ? "token-stores" : effect.token);
        break;
      case "save": fly(anchor(effect.at), $("db-sum"), "save", "", 450); break;
      case "flash": {
        const at = effect.at;
        if (at.kind === "cell") pulse(anchor(at), "flash");
        else if (at.kind === "pool") pulse($("beat"), "lit");
        else if (at.kind === "node") pulse(anchor(at), "spark");
        else if (at.kind === "upkeep") $("upd").querySelectorAll("i").forEach((i, n) => setTimeout(() => pulse(i, "lit"), n * 80));
        break;
      }
      case "probe": pulse(cellEls.get(String(effect.height)), "probe"); break;
      case "new_blocks":
        requestAnimationFrame(() => { for (let h = effect.from; h <= effect.to; h++) pulse(cellEls.get(String(h)), "enter"); });
        pulse(anchor({ kind: "node" }), "spark");
        break;
      case "drop":
        for (let h = effect.from; h <= effect.to; h++) {
          const cell = cellEls.get(String(h));
          if (!cell) continue;
          const [x, y] = centre(cell);
          const ghost = cell.cloneNode(true);
          ghost.classList.add("ghostcell");
          ghost.style.left = `${x - cell.offsetWidth / 2}px`;
          ghost.style.top = `${y - cell.offsetHeight / 2}px`;
          stage.appendChild(ghost);
          ghost.animate([{ transform: "none", opacity: 1 }, { transform: "translateY(40px) rotate(8deg)", opacity: 0 }], { duration: 750, easing: "ease-in" })
            .finished.then(() => ghost.remove(), () => ghost.remove());
        }
        break;
      default: break;
    }
  }

  // ---- the timeline ----
  // One bar: the whole history the page holds, every event on it. The
  // window over it (orange, full height) is the stretch being looked at:
  // drag its middle to move it, its handles to resize it. While its right
  // edge is at now it follows now and the page is live; the playback
  // position, a marker inside the window, shows only when it isn't.
  const canvas = $("tl"), ctx = canvas.getContext("2d");
  // The last five minutes at first: once the history is longer than the
  // window, there is room to move it.
  const win = { span: 5 * 60000, end: null };
  // The bar always spans 30 minutes, the history filling it from the
  // right. Its right edge is now while live, and stays where it was when
  // playback left live (moving on only as replay passes it).
  const AXIS = 30 * 60000;
  let frozenEnd = null;
  let hoverX = null, tlDirty = true;
  const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  function axisEnd() {
    if (frozenEnd === null) return engineNow();
    frozenEnd = Math.max(frozenEnd, head);
    return frozenEnd;
  }
  function axis() { const end = axisEnd(); return [end - AXIS, end]; }
  // The stretch the window can cover: the history held, on the bar.
  function range() {
    const [a, end] = axis(), start = Math.max(a, oldest ?? end - 60000);
    return [start, Math.max(end, start + MIN_SPAN)];
  }
  // The window, in engine milliseconds.
  function view_() {
    const [start, now] = range();
    const span = Math.min(win.span, now - start);
    const end = win.end == null ? now : Math.min(win.end, now);
    return [Math.max(start, end - span), Math.max(start + span, end)];
  }
  // A window as long as the history stays as long as it while it grows.
  function setWindow(end, span) {
    const [start, now] = range();
    win.span = span >= now - start - 250 ? Infinity : Math.max(MIN_SPAN, span);
    win.end = end == null || end >= now - 250 ? null : Math.max(start + win.span, end);
    tlDirty = true;
  }
  // Moves the window, keeping its length, so that `t` is inside it.
  function containPlayhead(t = head) {
    const [a, b] = view_();
    if (t < a) setWindow(t + (b - a) * 0.8, b - a);
    else if (t > b) setWindow(t + (b - a) * 0.2, b - a);
  }
  const xOf = (t, w) => { const [a, b] = axis(); return ((t - a) / (b - a)) * w; };
  const tOf = (x, w) => { const [a, b] = axis(); return a + (x / w) * (b - a); };
  // "45s", "11m 8s", "1h 5m".
  const ago = (ms) => {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m${s % 60 ? ` ${s % 60}s` : ""}`;
    return `${Math.floor(s / 3600)}h${Math.floor(s / 60) % 60 ? ` ${Math.floor(s / 60) % 60}m` : ""}`;
  };
  const clockAt = (t) => new Date(t - offset).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  function setReadout(text) { $("tl-text").textContent = text; }
  function headText() {
    if (mode === "live") return "Playback position: live, 1.5s behind the engine. Drag it back to pause there.";
    return `Playback position: ${ago(engineNow() - head)} ago (${clockAt(head)}). Drag it to scrub; Play replays from here.`;
  }
  function drawTimeline() {
    tlDirty = false;
    const dpr = devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
    if (!w) return;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr); }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const mid = h / 2, live = mode === "live", clock = performance.now();
    const [a, b] = view_(), [start, now] = range();
    // The history held fills the bar from the right. Before it (the engine
    // started less than 30 minutes ago) the bar is hatched, with a line
    // where the engine's record starts: nothing to show or move to there.
    ctx.fillStyle = css("--surface-sunken");
    const held = Math.max(0, xOf(start, w));
    ctx.fillRect(held, 0, w - held, h);
    if (held > 0) {
      ctx.save();
      ctx.beginPath(); ctx.rect(0, 0, held, h); ctx.clip();
      ctx.strokeStyle = css("--muted"); ctx.globalAlpha = 0.55; ctx.lineWidth = 1.25;
      ctx.beginPath();
      for (let x = -h; x < held; x += 7) { ctx.moveTo(x, h); ctx.lineTo(x + h, 0); }
      ctx.stroke();
      ctx.restore();
      ctx.globalAlpha = 0.7; ctx.fillStyle = css("--muted");
      ctx.fillRect(held - 0.5, 0, 1, h);
      ctx.globalAlpha = 1;
    }
    // Positions aren't rounded to pixels, so everything glides as time
    // passes; a new event fades in.
    const fade = (mark) => (mark.arrived ? Math.min(1, (clock - mark.arrived) / 400) : 1);
    const keys = [];
    ctx.strokeStyle = css("--muted"); ctx.lineWidth = 1;
    for (const mark of marks) {
      const x = xOf(mark.at_ms, w);
      if (x < -6 || x > w + 6) continue;
      if (mark.key) { keys.push([x, mark]); continue; }
      ctx.globalAlpha = (live || mark.at_ms <= head ? 0.75 : 0.3) * fade(mark);
      ctx.beginPath(); ctx.moveTo(x, mid - 6); ctx.lineTo(x, mid + 6); ctx.stroke();
    }
    for (const [x, mark] of keys) {
      ctx.beginPath(); ctx.arc(x, mid, 5, 0, Math.PI * 2);
      ctx.fillStyle = css(`--viz-tier-${mark.tier || "chain"}`);
      ctx.globalAlpha = (!live && mark.at_ms > head ? 0.4 : 1) * fade(mark);
      ctx.fill(); ctx.lineWidth = 1.5; ctx.strokeStyle = css("--paper-raised"); ctx.stroke();
    }
    ctx.globalAlpha = 1;
    ctx.strokeStyle = css("--line-strong"); ctx.strokeRect(0.5, 0.5, w - 1, h - 1);

    // The window and the playback position, as elements over the bar.
    const box = $("tl-win"), marker = $("tl-head");
    // Anchored by its right edge, so a window too short to grab grows to
    // the left, never past the bar's end.
    box.style.right = `${100 - (xOf(b, w) / w) * 100}%`;
    box.style.width = `${((xOf(b, w) - xOf(a, w)) / w) * 100}%`;
    const windowText = `${ago(now - a)} ago to ${win.end == null ? "now" : ago(now - b) + " ago"}`;
    for (const id of ["tl-win", "tl-from", "tl-to"]) $(id).setAttribute("aria-valuetext", windowText);
    marker.style.left = `${(xOf(head, w) / w) * 100}%`;
    marker.setAttribute("aria-valuetext", headText());

    const tip = $("tl-tipbox");
    let best = null;
    if (hoverX != null && !dragging) for (const k of keys) if (Math.abs(k[0] - hoverX) < 7 && (!best || Math.abs(k[0] - hoverX) < Math.abs(best[0] - hoverX))) best = k;
    const onHead = hoverX != null && Math.abs(xOf(head, w) - hoverX) < 6;
    const onVoid = !best && !onHead && hoverX != null && hoverX < held - 2;
    tip.hidden = !best && !onHead && !onVoid;
    if (onVoid) { tip.textContent = `No data before ${clockAt(start)}, when the engine started: nothing to show or move to there.`; tip.style.left = `${Math.max(220, Math.min(w - 220, hoverX))}px`; }
    else if (onHead) { tip.textContent = headText(); tip.style.left = `${Math.max(180, Math.min(w - 180, xOf(head, w)))}px`; }
    else if (best) { tip.textContent = best[1].text; tip.style.left = `${Math.max(140, Math.min(w - 140, best[0]))}px`; }

    setReadout(live ? "" : `${ago(engineNow() - head)} behind live`);
    // Five-minute marks back from the bar's right edge, which is now while
    // live and the moment playback left live otherwise.
    const [axisStart, end] = axis(), reallyNow = engineNow();
    const label = (t) => (reallyNow - t < 1000 ? "now" : `${ago(reallyNow - t)} ago`);
    let labels = `<span class="edge" style="left:0">${label(axisStart)}</span>`;
    for (let back = 5 * 60000; back < AXIS; back += 5 * 60000) {
      const p = ((AXIS - back) / AXIS) * 100;
      if ((p / 100) * w < 90 || ((100 - p) / 100) * w < 70) continue;
      labels += `<span style="left:${p}%">${label(end - back)}</span>`;
    }
    labels += `<span class="end" style="left:100%">${label(end)}</span>`;
    if ($("tl-axis").innerHTML !== labels) $("tl-axis").innerHTML = labels;
  }

  // Moving the window: by its middle, or one end by a handle. The window
  // never gets shorter than MIN_SPAN.
  function moveWindow(part, from, to) {
    const [start, now] = range();
    if (part === "move") setWindow(Math.min(now, Math.max(start + (to - from), to)), to - from);
    else if (part === "from") setWindow(to, to - Math.max(start, Math.min(from, to - MIN_SPAN)));
    else setWindow(Math.max(from + MIN_SPAN, Math.min(to, now)), Math.max(from + MIN_SPAN, Math.min(to, now)) - from);
  }

  // After the window moved: live it stays live only while it ends at now;
  // a window moved into the past pauses at its start, and a paused
  // playback position left outside it comes to its nearer edge.
  function settleWindow() {
    const [a, b] = view_();
    if (mode === "live" && win.end != null) { setMode("paused"); seek(a); }
    else if (mode !== "live" && (head < a || head > b)) seek(Math.max(a, Math.min(b, head)));
  }

  function startReplay() { setMode("replay"); queue = []; replayTo = head; fetchReplay(); }
  function togglePlay() { if (mode === "paused") startReplay(); else setMode("paused"); }

  let dragging = null;
  // A click on a recent round shows it in the round card; the chip goes
  // back to the live round.
  function wireRounds() {
    $("engine-round").addEventListener("click", async (e) => {
      if (e.target.closest("#round-resume")) {
        e.preventDefault();
        pinned = null;
        if (view) drawRound(view);
        return;
      }
      const bar = e.target.closest("a.rbar[data-round]");
      if (!bar) return;
      e.preventDefault();
      const response = await fetch(`/status/engine/round?network=${encodeURIComponent(network)}&number=${bar.dataset.round}`);
      if (!response.ok) return;
      pinned = await response.json();
      // The recent rounds hold still with it, as they were.
      if (view) { pinnedRibbon = view.ribbon; drawRound(view); }
    });
  }

  function wireTimeline() {
    const track = $("tl-track"), box = $("tl-win"), marker = $("tl-head");
    const tip = document.createElement("div");
    tip.className = "tl-tip"; tip.id = "tl-tipbox"; tip.hidden = true;
    track.appendChild(tip);
    const xAt = (e) => { const r = canvas.getBoundingClientRect(); return Math.max(0, Math.min(r.width, e.clientX - r.left)); };
    const tAt = (e) => tOf(xAt(e), canvas.clientWidth);

    // A drag of the window, a handle or the playback position. A press on
    // the window that doesn't move goes to that moment.
    const begin = (part) => (e) => {
      e.preventDefault(); e.stopPropagation();
      const [a, b] = view_();
      dragging = { part, t0: tAt(e), x0: e.clientX, a, b, moved: false };
      e.currentTarget.setPointerCapture(e.pointerId);
      box.classList.add("moving");
    };
    const follow = (e) => {
      hoverX = xAt(e); tlDirty = true;
      if (!dragging) return;
      if (!dragging.moved && Math.abs(e.clientX - dragging.x0) < 4) return;
      dragging.moved = true;
      const dt = tAt(e) - dragging.t0, [start, now] = range();
      if (dragging.part === "head") {
        // As if Pause were pressed, wherever it is let go; the window
        // follows it.
        if (mode !== "paused") setMode("paused");
        const t = tAt(e);
        containPlayhead(t);
        seek(t);
      } else if (dragging.part === "move") {
        const shift = Math.max(start - dragging.a, Math.min(now - dragging.b, dt));
        moveWindow("move", dragging.a + shift, dragging.b + shift);
      } else if (dragging.part === "from") moveWindow("from", dragging.a + dt, dragging.b);
      else moveWindow("to", dragging.a, dragging.b + dt);
    };
    const end = (e) => {
      if (!dragging) return;
      const { part, moved } = dragging;
      dragging = null;
      box.classList.remove("moving");
      if (part === "move" && !moved) { setMode("paused"); seek(tAt(e)); }
      else if (part !== "head") settleWindow();
    };
    box.addEventListener("pointerdown", begin("move"));
    $("tl-from").addEventListener("pointerdown", begin("from"));
    $("tl-to").addEventListener("pointerdown", begin("to"));
    marker.addEventListener("pointerdown", begin("head"));
    for (const el of [box, $("tl-from"), $("tl-to"), marker]) {
      el.addEventListener("pointermove", follow);
      el.addEventListener("pointerup", end);
      el.addEventListener("pointercancel", end);
    }
    // A press on the bar outside the window takes the window there, and
    // the playback position with it.
    canvas.addEventListener("pointerdown", (e) => {
      const [a, b] = view_(), t = tAt(e);
      moveWindow("move", t - (b - a) / 2, t + (b - a) / 2);
      setMode("paused");
      seek(t);
    });
    track.addEventListener("pointermove", (e) => { hoverX = xAt(e); tlDirty = true; });
    track.addEventListener("pointerleave", () => { hoverX = null; tlDirty = true; });

    const windowKeys = (part) => (e) => {
      const [a, b] = view_(), step = (b - a) / 10;
      const d = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
      if (!d) return;
      e.preventDefault(); e.stopPropagation();
      if (part === "move") { const [start, now] = range(); const s = Math.max(start - a, Math.min(now - b, d)); moveWindow("move", a + s, b + s); }
      else if (part === "from") moveWindow("from", a + d, b);
      else moveWindow("to", a, b + d);
      settleWindow();
    };
    $("tl-from").addEventListener("keydown", windowKeys("from"));
    $("tl-to").addEventListener("keydown", windowKeys("to"));
    box.addEventListener("keydown", (e) => {
      if (e.target !== box) return;
      const candidates = marks.filter((m) => e.shiftKey || m.key);
      if (e.key === "ArrowLeft") { const p = candidates.filter((m) => m.at_ms < head - 1).pop(); if (p) { setMode("paused"); containPlayhead(p.at_ms); seek(p.at_ms); } }
      else if (e.key === "ArrowRight") { const n = candidates.find((m) => m.at_ms > head + 1); if (n) { setMode("paused"); containPlayhead(n.at_ms); seek(n.at_ms); } }
      else if (e.key === "End") goLive();
      else if (e.key === " ") togglePlay();
      else return;
      e.preventDefault();
    });
    for (const radio of document.querySelectorAll('#tl-modes input[name="tl-mode"]')) {
      radio.addEventListener("change", () => {
        if (!radio.checked) return;
        if (radio.value === "live") goLive();
        else if (radio.value === "replay") startReplay();
        else setMode("paused");
      });
    }
    $("engine-events").addEventListener("click", (e) => {
      const row = e.target.closest("tr[data-at]");
      if (!row) return;
      setMode("paused");
      containPlayhead(Number(row.dataset.at));
      seek(Number(row.dataset.at));
    });
    document.querySelectorAll("#engine-filters input[data-tier]").forEach((input) => {
      input.addEventListener("change", () => {
        if (input.checked) filters.add(input.dataset.tier); else filters.delete(input.dataset.tier);
        drawEvents();
      });
    });
  }

  // ---- start ----
  if (!$("tl")) return; // the engine couldn't be read: nothing to follow
  $("engine-timeline").hidden = false;
  $("engine-filters").hidden = false;
  document.querySelectorAll(".engine-page .reload").forEach((el) => { el.hidden = true; });
  wireTimeline();
  wireRounds();
  const help = $("engine-help");
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && help.open) { help.open = false; help.querySelector("summary").focus(); } });
  document.addEventListener("pointerdown", (e) => { if (help.open && !help.contains(e.target)) help.open = false; });
  addEventListener("resize", () => { if (view) drawChain(view.chain); tlDirty = true; });
  document.addEventListener("visibilitychange", () => { if (!document.hidden && mode === "live") queue = queue.slice(-1); });
  setInterval(() => { if (mode !== "live") tlDirty = true; }, 1000);
  connect();
  requestAnimationFrame(frameLoop);
})();

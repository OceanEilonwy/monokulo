// The engine page's script (docs/engine_visualizer.md). Without it the page
// is a point-in-time view rendered by monokulo; with it, the page follows the
// network live.
//
// It draws, and nothing else: every word and figure comes from the server
// (`engine_view::present`), every state from its state machine
// (`engine_view::machine`). The server sends frames (what the page shows at a
// moment, and the effects that led there); this script plays them about 1.5 s
// behind the engine, animates the effects, and runs the timeline: click or
// drag on it to go to a moment, which it asks the server for
// (`/status/engine/at`, `/status/engine/replay`); the window under it chooses
// the part of the history drawn, by its middle and its two handles.
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
      for (const mark of frame.marks) marks.push(mark);
      if (mode === "live") queue.push(frame);
      tlDirty = true;
    });
    source.addEventListener("restarted", () => connect());
    source.addEventListener("unreachable", () => setReadout("The engine isn't answering; showing what was last seen"));
  }

  // ---- playback ----
  function frameLoop() {
    if (mode === "live") {
      head = Math.max(head, engineNow() - LAG);
      while (queue.length && queue[0].at_ms <= head) play(queue.shift());
      // Far behind (a hidden tab): skip to the newest.
      if (queue.length > 40) { const last = queue.pop(); queue = []; play(last, true); }
    } else if (mode === "replay") {
      head += 16;
      while (queue.length && queue[0].at_ms <= head) play(queue.shift());
      if (!queue.length && head >= replayTo) fetchReplay();
      if (head >= engineNow() - LAG) goLive();
      followPlayhead();
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

  function setMode(next) {
    mode = next;
    $("tl-play").textContent = mode === "paused" ? "Play" : "Pause";
    $("tl-live").disabled = mode === "live";
    $("tl-read").classList.toggle("paused", mode !== "live");
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

  function drawRound(v) {
    const box = $("engine-round");
    if (!box) return;
    let html = "";
    const round = v.round;
    if (round) {
      html += `<header class="round-head"><h2 id="h-round">${esc(round.title)}</h2><span class="engine-hint round-state">${esc(round.state)}</span></header><div class="lanes">`;
      for (const lane of round.lanes) {
        html += `<div class="lane-label"><span class="tierchip t-${lane.tier}">${esc(lane.name)}</span><small>${esc(lane.share)}</small></div><div class="track t-${lane.tier}">`;
        if (lane.reserved) html += `<div class="share" style="left:${pct(lane.reserved[0], round.scale_ms)}%;width:${pct(lane.reserved[1], round.scale_ms)}%"></div>`;
        for (const bar of lane.bars) html += `<div class="bar${bar.leftover ? " p2" : ""}" style="left:${pct(bar.start_ms, round.scale_ms)}%;width:${Math.max(0.5, pct(bar.ms, round.scale_ms))}%"></div>`;
        html += `<div class="playhead" style="left:${Math.min(99.5, pct(round.elapsed_ms, round.scale_ms))}%"></div></div><div class="outcome">`;
        if (lane.outcome) html += `<span class="engine-chip ${lane.outcome.tone}" title="${esc(lane.outcome.text)}">${esc(lane.outcome.text)}</span>`;
        html += "</div>";
      }
      html += `<div></div><div class="ruler"><span class="ruler-label" style="left:${Math.min(99.5, pct(round.elapsed_ms, round.scale_ms))}%">${esc(round.elapsed)}</span></div><div></div></div>`;
    } else {
      html += '<header><h2 id="h-round">Round</h2><span class="engine-hint">No round recorded yet.</span></header>';
    }
    html += '<div class="ribbon-row"><span class="engine-hint">Last rounds</span><div class="ribbon" id="ribbon" aria-label="Recent rounds">';
    for (const mark of v.ribbon) {
      if (mark.kind === "round") {
        html += `<span class="rbar" style="height:${mark.height}px" title="${esc(mark.title)}">${mark.parts.map(([tier, part]) => `<i class="t-${tier}" style="height:${(part * 100).toFixed(1)}%"></i>`).join("")}</span>`;
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
  const canvas = $("tl"), ctx = canvas.getContext("2d");
  const win = { span: 5 * 60000, end: null };
  let hoverX = null, scrubbing = false, tlDirty = true;
  const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  function range() {
    const now = engineNow(), start = oldest ?? now - 60000;
    return [start, now];
  }
  function view_() {
    const [start, now] = range();
    const span = Math.min(win.span, Math.max(MIN_SPAN, now - start));
    const end = win.end == null ? now : Math.min(win.end, now);
    return [Math.max(start, end - span), Math.max(start + span, end)];
  }
  function setWindow(end, span) {
    const [start, now] = range();
    win.span = Math.max(MIN_SPAN, Math.min(span, Math.max(MIN_SPAN, now - start)));
    win.end = end == null || end >= now - 250 ? null : Math.max(start + win.span, end);
    tlDirty = true;
  }
  function followPlayhead() {
    const [a, b] = view_();
    if (head > b || head < a) setWindow(head + win.span * 0.8, win.span);
  }
  const xOf = (t, w) => { const [a, b] = view_(); return ((t - a) / (b - a)) * w; };
  const tOf = (x, w) => { const [a, b] = view_(); return a + (x / w) * (b - a); };
  const ago = (ms) => { const s = Math.round(ms / 1000); return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min${s % 60 ? " " + (s % 60) + " s" : ""}`; };
  function setReadout(text) { $("tl-text").textContent = text; }
  function drawTimeline() {
    tlDirty = false;
    const dpr = devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
    if (!w) return;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr); }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const th = h, mid = th / 2;
    const [a, b] = view_(), [start, now] = range();
    ctx.fillStyle = css("--surface-sunken"); ctx.fillRect(0, 0, w, th);
    const xh = xOf(head, w), xl = xOf(now, w);
    ctx.fillStyle = css("--line"); ctx.fillRect(Math.max(0, xh), 0, Math.max(0, Math.min(w, xl) - Math.max(0, xh)), th);
    const keys = [];
    ctx.strokeStyle = css("--muted"); ctx.lineWidth = 1;
    for (const past of [true, false]) {
      ctx.beginPath();
      for (const mark of marks) {
        if (mark.at_ms < a || mark.at_ms > b || (mark.at_ms <= head) !== past) continue;
        const x = Math.round(xOf(mark.at_ms, w)) + 0.5;
        if (mark.key) { keys.push([x, mark]); continue; }
        ctx.moveTo(x, mid - 6); ctx.lineTo(x, mid + 6);
      }
      ctx.globalAlpha = past ? 0.75 : 0.3; ctx.stroke();
    }
    ctx.globalAlpha = 1;
    for (const [x, mark] of keys) {
      ctx.beginPath(); ctx.arc(x, mid, 5, 0, Math.PI * 2);
      ctx.fillStyle = css(`--viz-tier-${mark.tier || "chain"}`);
      ctx.globalAlpha = mark.at_ms > head ? 0.4 : 1;
      ctx.fill(); ctx.lineWidth = 1.5; ctx.strokeStyle = css("--paper-raised"); ctx.stroke();
    }
    ctx.globalAlpha = 1;
    ctx.fillStyle = css("--ink");
    if (xh >= 0 && xh <= w) {
      ctx.fillRect(Math.round(xh) - 1, 0, 2, th);
      ctx.beginPath(); ctx.moveTo(xh - 5, 0); ctx.lineTo(xh + 5, 0); ctx.lineTo(xh, 6); ctx.fill();
    } else {
      const edge = xh < 0 ? 0 : w, d = xh < 0 ? 9 : -9;
      ctx.beginPath(); ctx.moveTo(edge, mid); ctx.lineTo(edge + d, mid - 7); ctx.lineTo(edge + d, mid + 7); ctx.fill();
    }
    ctx.strokeStyle = css("--line-strong"); ctx.strokeRect(0.5, 0.5, w - 1, th - 1);
    const total = Math.max(1, now - start);
    drawOverview(start, total, a, b);
    const tip = $("tl-tipbox");
    let best = null;
    if (hoverX != null && !scrubbing) for (const k of keys) if (Math.abs(k[0] - hoverX) < 7 && (!best || Math.abs(k[0] - hoverX) < Math.abs(best[0] - hoverX))) best = k;
    tip.hidden = !best;
    if (best) { tip.textContent = best[1].text; tip.style.left = `${Math.max(140, Math.min(w - 140, best[0]))}px`; }
    const behind = now - head;
    setReadout(mode === "live" ? "Live, 1.5 s behind" : `${mode === "paused" ? "Paused" : "Replaying"}, ${ago(behind)} behind live`);
    canvas.setAttribute("aria-valuenow", String(Math.round(((head - start) / total) * 100)));
    canvas.setAttribute("aria-valuetext", $("tl-text").textContent);
    const span = b - a;
    const step = [5e3, 1e4, 15e3, 3e4, 6e4, 12e4, 3e5, 6e5].find((s) => span / s <= 7) || 6e5;
    let axis = "";
    for (let back = Math.max(step, Math.ceil((now - b) / step) * step); now - back >= a; back += step) {
      const p = ((now - back - a) / span) * 100;
      // Clear of the labels at either end.
      if ((p / 100) * w < 90 || ((100 - p) / 100) * w < 60) continue;
      axis += `<span style="left:${p}%">${back ? ago(back) + " ago" : "now"}</span>`;
    }
    const html = `<span style="left:0">${ago(now - a)} ago</span>${axis}<span style="left:100%">${win.end == null ? "now" : ago(now - b) + " ago"}</span>`;
    if ($("tl-axis").innerHTML !== html) $("tl-axis").innerHTML = html;
  }

  // The overview under the timeline: the whole history, its key events as
  // dots, the playback position, and the window drawn above.
  const over = $("tl-over"), octx = over.getContext("2d");
  function drawOverview(start, total, a, b) {
    const dpr = devicePixelRatio || 1, w = over.clientWidth, h = over.clientHeight;
    if (!w) return;
    if (over.width !== Math.round(w * dpr) || over.height !== Math.round(h * dpr)) { over.width = Math.round(w * dpr); over.height = Math.round(h * dpr); }
    octx.setTransform(dpr, 0, 0, dpr, 0, 0);
    octx.clearRect(0, 0, w, h);
    octx.fillStyle = css("--surface-sunken"); octx.fillRect(0, 0, w, h);
    const x = (t) => ((t - start) / total) * w;
    for (const mark of marks) {
      if (!mark.key) continue;
      octx.fillStyle = css(`--viz-tier-${mark.tier || "chain"}`);
      octx.fillRect(Math.round(x(mark.at_ms)) - 1, h / 2 - 2, 2, 4);
    }
    octx.fillStyle = css("--ink"); octx.fillRect(Math.round(x(head)) - 1, 0, 2, h);
    const box = $("tl-win");
    box.style.left = `${(x(a) / w) * 100}%`;
    box.style.width = `${(Math.max(0, x(b) - x(a)) / w) * 100}%`;
    const label = `${ago(engineNow() - a)} ago to ${win.end == null ? "now" : ago(engineNow() - b) + " ago"}`;
    for (const id of ["tl-win", "tl-from", "tl-to"]) $(id).setAttribute("aria-valuetext", label);
  }

  // Moving the window: by its middle, or one end by a handle. `end` null
  // follows live; the window never gets shorter than MIN_SPAN.
  function moveWindow(part, from, to) {
    const [start, now] = range();
    if (part === "move") setWindow(Math.min(now, Math.max(start + (to - from), to)), to - from);
    else if (part === "from") setWindow(to, to - Math.max(start, Math.min(from, to - MIN_SPAN)));
    else setWindow(Math.max(from + MIN_SPAN, Math.min(to, now)), Math.max(from + MIN_SPAN, Math.min(to, now)) - from);
  }

  function wireBrush() {
    const brush = $("tl-brush"), box = $("tl-win");
    const tAt = (clientX) => {
      const r = brush.getBoundingClientRect(), [start, now] = range();
      return start + (Math.max(0, Math.min(r.width, clientX - r.left)) / Math.max(1, r.width)) * (now - start);
    };
    let drag = null;
    const begin = (part) => (e) => {
      e.preventDefault(); e.stopPropagation();
      const [a, b] = view_();
      drag = { part, t0: tAt(e.clientX), a, b };
      e.currentTarget.setPointerCapture(e.pointerId);
      box.classList.add("moving");
    };
    const follow = (e) => {
      if (!drag) return;
      const dt = tAt(e.clientX) - drag.t0, [start, now] = range();
      if (drag.part === "move") {
        const shift = Math.max(start - drag.a, Math.min(now - drag.b, dt));
        moveWindow("move", drag.a + shift, drag.b + shift);
      } else if (drag.part === "from") moveWindow("from", drag.a + dt, drag.b);
      else moveWindow("to", drag.a, drag.b + dt);
    };
    const end = () => { drag = null; box.classList.remove("moving"); };
    box.addEventListener("pointerdown", begin("move"));
    $("tl-from").addEventListener("pointerdown", begin("from"));
    $("tl-to").addEventListener("pointerdown", begin("to"));
    for (const el of [box, $("tl-from"), $("tl-to")]) {
      el.addEventListener("pointermove", follow);
      el.addEventListener("pointerup", end);
      el.addEventListener("pointercancel", end);
    }
    // A press on the bar outside the window centres the window there.
    brush.addEventListener("pointerdown", (e) => {
      if (e.target !== brush && e.target !== over) return;
      const [a, b] = view_(), t = tAt(e.clientX);
      moveWindow("move", t - (b - a) / 2, t + (b - a) / 2);
    });
    const keys = (part) => (e) => {
      const [a, b] = view_(), step = (b - a) / 10;
      const d = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
      if (!d) return;
      e.preventDefault(); e.stopPropagation();
      if (part === "move") { const [start, now] = range(); const s = Math.max(start - a, Math.min(now - b, d)); moveWindow("move", a + s, b + s); }
      else if (part === "from") moveWindow("from", a + d, b);
      else moveWindow("to", a, b + d);
    };
    box.addEventListener("keydown", keys("move"));
    $("tl-from").addEventListener("keydown", keys("from"));
    $("tl-to").addEventListener("keydown", keys("to"));
  }

  function wireTimeline() {
    const tip = document.createElement("div");
    tip.className = "tl-tip"; tip.id = "tl-tipbox"; tip.hidden = true;
    $("tl-track").appendChild(tip);
    const at = (e) => { const r = canvas.getBoundingClientRect(); return Math.max(0, Math.min(r.width, e.clientX - r.left)); };
    // Pressing goes to that moment; dragging scrubs through the moments.
    canvas.addEventListener("pointerdown", (e) => {
      scrubbing = true;
      canvas.setPointerCapture(e.pointerId);
      canvas.classList.add("scrubbing");
      setMode("paused");
      seek(tOf(at(e), canvas.clientWidth));
    });
    canvas.addEventListener("pointermove", (e) => {
      hoverX = at(e); tlDirty = true;
      if (scrubbing) seek(tOf(hoverX, canvas.clientWidth));
    });
    const stop = () => { scrubbing = false; canvas.classList.remove("scrubbing"); };
    canvas.addEventListener("pointerup", stop);
    canvas.addEventListener("pointercancel", stop);
    canvas.addEventListener("pointerleave", () => { hoverX = null; tlDirty = true; });
    canvas.addEventListener("keydown", (e) => {
      const candidates = marks.filter((m) => e.shiftKey || m.key);
      if (e.key === "ArrowLeft") { const p = candidates.filter((m) => m.at_ms < head - 1).pop(); if (p) { setMode("paused"); seek(p.at_ms); followPlayhead(); } }
      else if (e.key === "ArrowRight") { const n = candidates.find((m) => m.at_ms > head + 1); if (n) { setMode("paused"); seek(n.at_ms); followPlayhead(); } }
      else if (e.key === "End") goLive();
      else if (e.key === " ") $("tl-play").click();
      else return;
      e.preventDefault();
    });
    $("tl-play").addEventListener("click", () => {
      if (mode === "paused") { setMode("replay"); queue = []; replayTo = head; fetchReplay(); }
      else { setMode("paused"); }
    });
    $("tl-live").addEventListener("click", goLive);
    $("engine-events").addEventListener("click", (e) => {
      const row = e.target.closest("tr[data-at]");
      if (!row) return;
      setMode("paused");
      seek(Number(row.dataset.at));
      followPlayhead();
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
  wireBrush();
  const help = $("engine-help");
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && help.open) { help.open = false; help.querySelector("summary").focus(); } });
  document.addEventListener("pointerdown", (e) => { if (help.open && !help.contains(e.target)) help.open = false; });
  addEventListener("resize", () => { if (view) drawChain(view.chain); tlDirty = true; });
  document.addEventListener("visibilitychange", () => { if (!document.hidden && mode === "live") queue = queue.slice(-1); });
  connect();
  requestAnimationFrame(frameLoop);
})();

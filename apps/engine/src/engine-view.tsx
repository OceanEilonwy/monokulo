// The engine page's app: it follows one
// network's scanner live, about 1.5 s behind the engine. Monokulo sends data
// (wire.ts): the state machine's state at moments, the effects and marks
// that led there, and every few seconds the machine's figures; the app
// writes every word (present.ts, machine.ts) and draws it (App.tsx).
//
// This is the part that keeps time, and the views read what it sets
// (state.ts): it plays the frames as they come, animates their effects, and
// runs the timeline: one bar of the whole history, with a window over it to
// move and resize, and inside the window, once off live, the playback
// position; each moment is asked of monokulo (`/status/engine/at`,
// `/status/engine/replay`). The bar is a canvas, drawn here as time passes.
//
// The page never zooms on the mouse wheel: scrolling scrolls the page.
import './engine.css';
import { render } from '@solidjs/web';
import { App } from './App.tsx';
import { machineView } from './machine.ts';
import { markText, present, presentRound } from './present.ts';
import {
  setCatchingUp, setHead, setLive, setMachine, setMachineAge, setMarks, setNetwork, setPinned, setView, view,
} from './state.ts';
import type { Anchor, Call, Effect, Frame, Machine, Mark, Start, Timed } from './wire.ts';

type Mode = 'live' | 'paused' | 'replay';

(() => {
  const root = document.getElementById('engine-app');
  const networkSelect = document.getElementById('engine-network') as HTMLSelectElement | null;
  // Picking a network goes there; Go is only for a page without JavaScript.
  networkSelect?.addEventListener('change', () => networkSelect.form?.requestSubmit());
  if (!root || !window.EventSource) return;
  const network = root.dataset.network;
  if (!network) return;
  setNetwork(network);

  const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const LAG = 1500;
  const MIN_SPAN = 5000;
  const REPLAY_MS = 10000;
  // How long things take to cross the page: slow enough to follow by eye.
  const FLY_MS = 1600; // a payment, stores moving
  const CALL_MS = 1300; // a call to the node
  const SAVE_MS = 1100; // a save flying to the database
  const DROP_MS = 1400; // a rewound block falling away
  // Effects of one frame are spread out by their events' times, doubled, up
  // to this, so a burst plays one after another.
  const STAGGER_MS = 1000;
  const PULSE_MS = 1200; // a block or the node lighting up
  const MAX_MARKS = 50000; // same bound as the engine activity record
  const MAX_FRAMES = 40;
  const DRAW_INTERVAL = 250;
  const TIMELINE_INTERVAL = 100;
  const MAX_EFFECTS = 30; // played from one frame at most
  // The bar always spans 30 minutes, the history filling it from the right.
  const AXIS = 30 * 60000;

  // ---- state ----
  let offset = 0; // engine clock minus ours
  let oldest: number | null = null; // the oldest moment the history has
  let marks: Mark[] = []; // every mark, oldest first
  let queue: Frame[] = []; // frames waiting to be played, oldest first
  let head = 0; // the moment drawn, engine milliseconds
  let mode: Mode = 'live';
  let replayTo = 0;
  let lastSeq = -1;
  let replayFetching = false;
  let source: EventSource | null = null;
  let animationFrame = 0, refreshTimer = 0, stopped = false;
  let lastDraw = 0, lastTimelineDraw = 0;
  let lastFrame: Frame | null = null; // the newest frame the stream sent
  let machineAt: number | null = null;
  let unreachable = false; // the engine didn't answer the last time it was asked
  // Each element's passing animation classes, and when each comes off.
  const pulsing = new WeakMap<Element, Map<string, ReturnType<typeof setTimeout>>>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const pendingFetches = new Set<AbortController>();
  const engineNow = () => Date.now() + offset;

  root.replaceChildren();
  render(() => <App actions={{ pick, pin, resume }} helpSlot={document.getElementById('engine-help-slot')} />, root);
  const stage = $('engine-stage');
  const canvas = $<HTMLCanvasElement>('tl');
  const ctx = canvas.getContext('2d')!;

  function later(callback: () => void, delay: number) {
    if (timers.size >= 120) return;
    const timer = setTimeout(() => { timers.delete(timer); if (!stopped && !document.hidden) callback(); }, delay);
    timers.add(timer);
  }
  function clearEffects() {
    for (const timer of timers) clearTimeout(timer);
    timers.clear();
    clearTokens();
  }
  // Reload is in the page's header, above the app.
  function showRefresh(show: boolean) {
    document.querySelectorAll<HTMLElement>('.engine-page [data-reload]').forEach((el) => { el.hidden = !show; });
  }
  async function readFrame(url: string) {
    const controller = new AbortController();
    pendingFetches.add(controller);
    try { return await fetch(url, { signal: controller.signal }); } finally { pendingFetches.delete(controller); }
  }
  function pruneMarks() {
    const keep = engineNow() - AXIS;
    let remove = Math.max(0, marks.length - MAX_MARKS);
    while (remove < marks.length && marks[remove].at_ms < keep) remove++;
    if (remove) marks.splice(0, remove);
    // The engine keeps the last AXIS of its record: nothing before that
    // can be shown, whatever marks the page still holds.
    if (oldest !== null) oldest = Math.max(oldest, keep);
    // Paused or replaying long enough, the playback position falls out of
    // the history: it comes up to its oldest moment.
    if (mode !== 'live' && oldest !== null && head < oldest) void seek(oldest);
  }
  /** Draws the state of `frame`: the views follow. Returns whether the scan
   *  is catching up thousands of blocks, when animations are left out. */
  function draw(frame: Frame): boolean {
    const chain = frame.state.chain;
    const catching = chain.tip !== null && chain.high_water !== null && chain.tip - chain.high_water > 1000;
    setCatchingUp(catching);
    if (catching) clearTokens();
    setView(present(frame.state, frame.tuning));
    return catching;
  }

  // ---- the stream ----
  function connect() {
    source?.close();
    source = new EventSource(`/status/engine/events?network=${encodeURIComponent(network!)}`);
    source.addEventListener('history', (e) => {
      const start: Start = JSON.parse(e.data);
      unreachable = false;
      offset = start.engine_now_ms - Date.now();
      oldest = start.oldest_ms;
      marks = start.marks.slice(-MAX_MARKS);
      pruneMarks();
      clearEffects();
      showRefresh(false);
      lastSeq = marks.length ? marks[marks.length - 1].seq : -1;
      setMarks(marks);
      queue = [];
      lastFrame = start.frame;
      tlDirty = true;
      // A reconnect while paused or replaying leaves the view where it is;
      // replay asks for its next stretch again from where it has got.
      if (mode === 'replay') replayTo = head;
      if (mode !== 'live') return;
      head = start.frame.at_ms;
      setHead(head);
      draw(start.frame);
    });
    source.addEventListener('frame', (e) => {
      const frame: Frame = JSON.parse(e.data);
      unreachable = false;
      if (oldest === null) oldest = frame.at_ms;
      const arrived = performance.now();
      // A frame can repeat marks the history already gave (it was sent while
      // the history was being read): each is taken once.
      for (const mark of frame.marks) {
        if (mark.seq <= lastSeq) continue;
        mark.arrived = arrived;
        marks.push(mark);
        lastSeq = mark.seq;
      }
      // What the engine no longer keeps, the page lets go of too.
      pruneMarks();
      setMarks(marks);
      lastFrame = frame;
      if (mode === 'live') {
        queue.push(frame);
        if (document.hidden) queue = queue.slice(-1);
        else if (queue.length > MAX_FRAMES) queue.splice(0, queue.length - MAX_FRAMES);
      }
      tlDirty = true;
    });
    source.addEventListener('machine', (e) => drawMachine(JSON.parse(e.data)));
    source.addEventListener('restarted', () => connect());
    // Said under the playback control until the engine answers again.
    source.addEventListener('unreachable', () => {
      showRefresh(true);
      unreachable = true;
      tlDirty = true;
    });
    source.addEventListener('error', () => showRefresh(true));
  }

  // ---- playback ----
  let lastTick = performance.now();
  function frameLoop(tick = performance.now()) {
    const dt = Math.min(250, tick - lastTick);
    lastTick = tick;
    if (stopped) return;
    if (document.hidden) { animationFrame = requestAnimationFrame(frameLoop); return; }
    if (mode === 'live') {
      head = Math.max(head, engineNow() - LAG);
      if (tick - lastDraw >= DRAW_INTERVAL) playDue(tick);
    } else if (mode === 'replay') {
      head += dt;
      if (tick - lastDraw >= DRAW_INTERVAL) playDue(tick);
      if (!queue.length && head >= replayTo) void fetchReplay();
      // Replay plays the window: it pauses at the window's end, or goes live
      // when the window ends at now.
      const [, b] = windowSpan();
      if (win.end != null && head >= b) { head = b; setMode('paused'); } else if (head >= engineNow() - LAG) goLive();
    }
    if (tick - lastTimelineDraw >= TIMELINE_INTERVAL && (tlDirty || timelineMoved())) { drawTimeline(); lastTimelineDraw = tick; }
    // Wherever playback moved it this frame: playing, a seek, going live.
    setHead(head);
    animationFrame = requestAnimationFrame(frameLoop);
  }

  /** Plays the newest of the frames due by the playback position, the
   *  others' effects left out. */
  function playDue(tick: number) {
    let ready = 0;
    while (ready < queue.length && queue[ready].at_ms <= head) ready++;
    if (!ready) return;
    const frame = queue[ready - 1];
    queue.splice(0, ready);
    play(frame, ready > 1);
    lastDraw = tick;
  }

  function play(frame: Frame, quietly: boolean) {
    const catching = draw(frame);
    if (!quietly && !catching && !document.hidden && !reduced) {
      const first = frame.effects.length ? frame.effects[0].at_ms : 0;
      // A catch-up can commit fifty blocks in one frame: a save or flash on
      // the same thing plays once, and at most MAX_EFFECTS play.
      const seen = new Set<string>(), effects: Timed[] = [];
      for (const timed of frame.effects) {
        const kind = timed.effect.kind;
        if (kind === 'save' || kind === 'flash' || kind === 'probe') {
          const key = JSON.stringify(timed.effect);
          if (seen.has(key)) continue;
          seen.add(key);
        }
        if (effects.length < MAX_EFFECTS) effects.push(timed);
      }
      for (const timed of effects) {
        const delay = Math.min(STAGGER_MS, Math.max(0, timed.at_ms - first) * 2);
        later(() => animate(timed.effect), delay);
      }
    }
  }

  // One request at a time: while one is out, a scrub only notes where it
  // got to, and that is asked for next. What arrives is drawn unless the
  // page went live meanwhile.
  let seeking = false, seekWanted: number | null = null;
  async function seek(t: number) {
    t = Math.max(oldest ?? t, Math.min(t, engineNow() - LAG));
    head = t;
    queue = [];
    tlDirty = true;
    seekWanted = t;
    if (seeking) return;
    seeking = true;
    try {
      while (seekWanted !== null) {
        const wanted: number = seekWanted;
        seekWanted = null;
        try {
          const response = await readFrame(`/status/engine/at?network=${encodeURIComponent(network!)}&ms=${Math.round(wanted)}`);
          if (!response.ok) continue;
          const frame: Frame = await response.json();
          if (mode === 'live') break;
          clearTokens();
          draw(frame);
        } catch {
          // Not answered (or the page is going): the next scrub asks again.
        }
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
      const response = await readFrame(`/status/engine/replay?network=${encodeURIComponent(network!)}&from=${from}&to=${to}`);
      if (!response.ok) throw new Error(`replay: ${response.status}`);
      if (mode === 'replay') {
        queue = ((await response.json()) as Frame[]).filter((f) => f.at_ms > head);
        replayTo = to;
      }
    } catch {
      // The engine (or monokulo) didn't answer: pause, rather than ask again
      // every frame.
      if (mode === 'replay') setMode('paused');
    } finally {
      replayFetching = false;
    }
  }

  // The playback control follows the mode: Play only off live. Leaving live
  // freezes the timeline's axis where it was; going live lets it go.
  function setMode(next: Mode) {
    if (mode === 'live' && next !== 'live') frozenEnd = engineNow();
    if (next === 'live') frozenEnd = null;
    if (mode !== next) clearEffects();
    mode = next;
    setLive(mode === 'live');
    const select = $<HTMLSelectElement>('tl-mode');
    select.value = mode;
    const replay = select.querySelector<HTMLOptionElement>('[value="replay"]');
    if (replay) replay.disabled = mode === 'live';
    // The control drawn over the select reads it again on `input` (the
    // page acts only on `change`).
    select.dispatchEvent(new Event('input', { bubbles: true }));
    tlDirty = true;
  }

  // Frames keep arriving while paused, so going live draws the newest rather
  // than downloading the whole history again.
  function goLive() {
    clearEffects();
    setMode('live');
    setWindow(null, win.span);
    queue = [];
    if (lastFrame) {
      head = Math.max(lastFrame.at_ms, engineNow() - LAG);
      clearTokens();
      draw(lastFrame);
    }
  }

  // ---- what the views ask for ----
  /** An event in the table: paused, and the page as it was then. */
  function pick(atMs: number) {
    setMode('paused');
    containPlayhead(atMs);
    void seek(atMs);
  }
  /** A past round, from the recent rounds: the round card shows it, the
   *  recent rounds holding still with it, as they were. */
  // The last round asked for: an answer to an earlier one is let go.
  let pinAsked = 0;
  async function pin(number: number) {
    const asked = ++pinAsked;
    try {
      const response = await readFrame(`/status/engine/round?network=${encodeURIComponent(network!)}&number=${number}`);
      if (!response.ok) return;
      const frame: Frame = await response.json();
      const round = presentRound(frame.state, frame.tuning);
      if (round && asked === pinAsked) setPinned({ round, ribbon: view()?.ribbon ?? [] });
    } catch {
      // Not answered (or the page is going): the live round stays.
    }
  }
  function resume() {
    setPinned(null);
  }

  // ---- the "Machine and links" strip and the Scanning panel ----
  // Each `machine` event redraws them; the chip says how long ago the engine
  // reported the figures.
  function drawMachine(figures: Machine) {
    setMachine(machineView(figures));
    machineAt = figures.at_unix * 1000;
    drawMachineAge();
  }
  function drawMachineAge() {
    if (machineAt === null) return;
    const ms = Math.max(0, engineNow() - machineAt);
    setMachineAge(`live · ${ms < 60000 ? `${Math.round(ms / 1000)} s` : ago(ms)} ago`);
  }

  // ---- animations: effects, never state ----
  const cellAt = (height: number) => document.querySelector<HTMLElement>(`#cells [data-h="${height}"]`);
  function anchor(a: Anchor | undefined): HTMLElement | null {
    if (!a) return null;
    switch (a.kind) {
      case 'node': return document.querySelector('#nodes .node:has(.engine-chip.ok)') ?? document.querySelector('#nodes .node');
      case 'cell': return cellAt(a.id) ?? document.querySelector('#cells .brk');
      case 'group': return document.querySelector(`#pills [data-id="${a.id}"]`);
      case 'pool': return $('pool-dots');
      case 'reorg': return document.querySelector('#d-reorg summary');
      case 'orders': return $('orders-sum');
      case 'database': return $('db-sum');
      case 'upkeep': return $('upd');
    }
  }
  function centre(el: HTMLElement): [number, number] {
    const s = stage.getBoundingClientRect(), r = el.getBoundingClientRect();
    return [r.left + r.width / 2 - s.left, r.top + r.height / 2 - s.top];
  }
  function fly(from: HTMLElement | null, to: HTMLElement | null, cls: string, label: string, ms = FLY_MS) {
    if (!from || !to) return;
    const [x0, y0] = centre(from), [x1, y1] = centre(to);
    const token = document.createElement('div');
    token.className = `token ${cls}`;
    if (label) token.textContent = label;
    token.style.left = `${x0}px`;
    token.style.top = `${y0}px`;
    stage.appendChild(token);
    const dx = x1 - x0, dy = y1 - y0, lift = Math.min(70, 16 + Math.abs(dx) / 10);
    const animation = token.animate([
      { transform: 'translate(-50%,-50%)', opacity: 0 },
      { transform: `translate(-50%,-50%) translate(${dx * 0.5}px,${dy * 0.5 - lift}px)`, opacity: 1, offset: 0.5 },
      { transform: `translate(-50%,-50%) translate(${dx}px,${dy}px)`, opacity: 1 },
    ], { duration: ms, easing: 'cubic-bezier(.45,0,.35,1)' });
    animation.finished.then(() => token.remove(), () => token.remove());
  }
  function pulse(el: Element | null, cls: string) {
    if (!el) return;
    el.classList.remove(cls);
    void (el as HTMLElement).offsetWidth;
    el.classList.add(cls);
    // Not through `later`: what clears the effects (a mode change, a hidden
    // page) must not leave the class on for good.
    const pulses = pulsing.get(el) ?? new Map<string, ReturnType<typeof setTimeout>>();
    clearTimeout(pulses.get(cls));
    pulses.set(cls, setTimeout(() => { el.classList.remove(cls); pulses.delete(cls); }, PULSE_MS));
    pulsing.set(el, pulses);
  }
  function clearTokens() {
    stage.querySelectorAll('.token, .ghostcell').forEach((el) => el.remove());
  }
  function callLabel(call: Call) {
    switch (call.kind) {
      case 'block_hash': return 'hash check';
      case 'blocks': return call.count === 1 ? '1 block' : `${call.count} blocks`;
      case 'pool': return 'pool';
      default: return 'txs';
    }
  }
  // The cells drawn for blocks `from` to `to`: a node far ahead can announce
  // millions of blocks, and only the drawn ones are animated.
  function drawnCells(from: number, to: number) {
    return [...document.querySelectorAll<HTMLElement>('#cells [data-h]')].filter((cell) => {
      const h = Number(cell.dataset.h);
      return h >= from && h <= to;
    });
  }
  function animate(effect: Effect) {
    switch (effect.kind) {
      case 'packet': {
        const cls = effect.call.kind === 'block_hash' || effect.call.kind === 'transactions' ? 'pkt chain' : effect.call.kind === 'pool' ? 'pkt mempool' : 'pkt';
        fly(anchor({ kind: 'node' }), anchor(effect.to), cls, callLabel(effect.call), CALL_MS);
        break;
      }
      case 'fly':
        fly(anchor(effect.from), anchor(effect.to), effect.token === 'stores' ? 'token-stores' : effect.token, '');
        break;
      case 'save': fly(anchor(effect.at), $('db-sum'), 'save', '', SAVE_MS); break;
      case 'flash': {
        const at = effect.at;
        if (at.kind === 'cell') pulse(anchor(at), 'flash');
        else if (at.kind === 'pool') pulse($('beat'), 'lit');
        else if (at.kind === 'node') pulse(anchor(at), 'spark');
        else if (at.kind === 'upkeep') $('upd').querySelectorAll('i').forEach((i, n) => later(() => pulse(i, 'lit'), n * 80));
        break;
      }
      case 'probe': pulse(cellAt(effect.height), 'probe'); break;
      case 'new_blocks':
        requestAnimationFrame(() => { for (const cell of drawnCells(effect.from, effect.to)) pulse(cell, 'enter'); });
        pulse(anchor({ kind: 'node' }), 'spark');
        break;
      case 'drop':
        for (const cell of drawnCells(effect.from, effect.to)) {
          const [x, y] = centre(cell);
          const ghost = cell.cloneNode(true) as HTMLElement;
          ghost.classList.add('ghostcell');
          ghost.style.left = `${x - cell.offsetWidth / 2}px`;
          ghost.style.top = `${y - cell.offsetHeight / 2}px`;
          stage.appendChild(ghost);
          ghost.animate([{ transform: 'none', opacity: 1 }, { transform: 'translateY(40px) rotate(8deg)', opacity: 0 }], { duration: DROP_MS, easing: 'ease-in' })
            .finished.then(() => ghost.remove(), () => ghost.remove());
        }
        break;
    }
  }

  // ---- the timeline ----
  // One bar: the whole history the page holds, every event on it. The window
  // over it (orange, full height) is the stretch being looked at: drag its
  // middle to move it, its handles to resize it. While its right edge is at
  // now it follows now and the page is live; the playback position, a marker
  // inside the window, shows only when it isn't.
  // The last five minutes at first: once the history is longer than the
  // window, there is room to move it.
  const win: { span: number; end: number | null } = { span: 5 * 60000, end: null };
  // The bar's right edge is now while live, and stays where it was when
  // playback left live (moving on only as replay passes it).
  let frozenEnd: number | null = null;
  let hoverX: number | null = null, tlDirty = true;
  // Theme colours, read once and again when the theme changes, not on every
  // frame.
  const cssCache = new Map<string, string>();
  const css = (name: string) => {
    if (!cssCache.has(name)) cssCache.set(name, getComputedStyle(document.documentElement).getPropertyValue(name).trim());
    return cssCache.get(name)!;
  };
  const themeChanged = () => { cssCache.clear(); tlDirty = true; };
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', themeChanged);
  const themeObserver = new MutationObserver(themeChanged);
  function axisEnd() {
    if (frozenEnd === null) return engineNow();
    frozenEnd = Math.max(frozenEnd, head);
    return frozenEnd;
  }
  function axis(): [number, number] { const end = axisEnd(); return [end - AXIS, end]; }
  // The stretch the window can cover: the history held, on the bar.
  function range(): [number, number] {
    const [a, end] = axis(), start = Math.max(a, oldest ?? end - 60000);
    return [start, Math.max(end, start + MIN_SPAN)];
  }
  // The window, in engine milliseconds.
  function windowSpan(): [number, number] {
    const [start, now] = range();
    const span = Math.min(win.span, now - start);
    const end = win.end == null ? now : Math.min(win.end, now);
    return [Math.max(start, end - span), Math.max(start + span, end)];
  }
  // A window as long as the history stays as long as it while it grows.
  function setWindow(end: number | null, span: number) {
    const [start, now] = range();
    win.span = span >= now - start - 250 ? Infinity : Math.max(MIN_SPAN, span);
    win.end = end == null || end >= now - 250 ? null : Math.max(start + win.span, end);
    tlDirty = true;
  }
  // Moves the window, keeping its length, so that `t` is inside it.
  function containPlayhead(t = head) {
    const [a, b] = windowSpan();
    if (t < a) setWindow(t + (b - a) * 0.8, b - a);
    else if (t > b) setWindow(t + (b - a) * 0.2, b - a);
  }
  const tOf = (x: number, w: number) => { const [a, b] = axis(); return a + (x / w) * (b - a); };
  // "45s", "11m 8s", "1h 5m".
  function ago(ms: number) {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m${s % 60 ? ` ${s % 60}s` : ''}`;
    return `${Math.floor(s / 3600)}h${Math.floor(s / 60) % 60 ? ` ${Math.floor(s / 60) % 60}m` : ''}`;
  }
  const clockAt = (t: number) => new Date(t - offset).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  function setReadout(text: string) { const el = $('tl-text'); if (el.textContent !== text) el.textContent = text; }
  const UNREACHABLE = "The engine isn't answering; showing what was last seen";
  function headText() {
    if (mode === 'live') return 'Playback position: live, 1.5s behind the engine. Drag it back to pause there.';
    return `Playback position: ${ago(engineNow() - head)} ago (${clockAt(head)}). Drag it to scrub; Play replays from here.`;
  }
  // The timeline is redrawn when something on it moved by half a pixel or
  // more (30 minutes over the bar is about a second a pixel), or while an
  // event fades in; not every frame.
  let drawn = { end: 0, head: 0, w: 0, fading: false };
  function timelineMoved() {
    const w = canvas.clientWidth;
    const halfPixel = AXIS / Math.max(1, w) / 2;
    return drawn.fading || w !== drawn.w
      || Math.abs(axisEnd() - drawn.end) >= halfPixel || Math.abs(head - drawn.head) >= halfPixel;
  }
  // Sets an attribute only when it changes.
  const setAttr = (el: Element, name: string, value: string) => { if (el.getAttribute(name) !== value) el.setAttribute(name, value); };
  function drawTimeline() {
    tlDirty = false;
    const dpr = devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
    if (!w) return;
    // The bar's scale, once per draw.
    const [axisStart, axisStop] = axis(), perMs = w / (axisStop - axisStart);
    const X = (t: number) => (t - axisStart) * perMs;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const mid = h / 2, isLive = mode === 'live', clock = performance.now();
    const [a, b] = windowSpan(), [start, now] = range();
    // The history held fills the bar from the right. Before it (the engine
    // started less than 30 minutes ago) the bar is hatched, with a line where
    // the engine's record starts: nothing to show or move to there.
    ctx.fillStyle = css('--surface-sunken');
    const held = Math.max(0, X(start));
    ctx.fillRect(held, 0, w - held, h);
    if (held > 0) {
      ctx.save();
      ctx.beginPath(); ctx.rect(0, 0, held, h); ctx.clip();
      ctx.strokeStyle = css('--muted'); ctx.globalAlpha = 0.55; ctx.lineWidth = 1.25;
      ctx.beginPath();
      for (let x = -h; x < held; x += 7) { ctx.moveTo(x, h); ctx.lineTo(x + h, 0); }
      ctx.stroke();
      ctx.restore();
      ctx.globalAlpha = 0.7; ctx.fillStyle = css('--muted');
      ctx.fillRect(held - 0.5, 0, 1, h);
      ctx.globalAlpha = 1;
    }
    // Positions aren't rounded to pixels, so everything glides as time
    // passes; a new event fades in.
    const fade = (mark: Mark) => (mark.arrived ? Math.min(1, (clock - mark.arrived) / 400) : 1);
    let fading = false;
    const keys: [number, Mark, number][] = [];
    // Plain events in two strokes (before and after the playback position);
    // only one still fading in is drawn on its own.
    ctx.strokeStyle = css('--muted'); ctx.lineWidth = 1;
    const past = new Path2D(), future = new Path2D();
    for (const mark of marks) {
      const x = X(mark.at_ms);
      if (x < -6 || x > w + 6) continue;
      const f = fade(mark);
      if (f < 1) fading = true;
      if (mark.key) { keys.push([x, mark, f]); continue; }
      const before = isLive || mark.at_ms <= head;
      if (f < 1) {
        ctx.globalAlpha = (before ? 0.75 : 0.3) * f;
        ctx.beginPath(); ctx.moveTo(x, mid - 6); ctx.lineTo(x, mid + 6); ctx.stroke();
        continue;
      }
      const path = before ? past : future;
      path.moveTo(x, mid - 6); path.lineTo(x, mid + 6);
    }
    ctx.globalAlpha = 0.75; ctx.stroke(past);
    ctx.globalAlpha = 0.3; ctx.stroke(future);
    for (const [x, mark, f] of keys) {
      ctx.beginPath(); ctx.arc(x, mid, 5, 0, Math.PI * 2);
      ctx.fillStyle = css(`--viz-tier-${mark.tier || 'chain'}`);
      ctx.globalAlpha = (!isLive && mark.at_ms > head ? 0.4 : 1) * f;
      ctx.fill(); ctx.lineWidth = 1.5; ctx.strokeStyle = css('--paper-raised'); ctx.stroke();
    }
    ctx.globalAlpha = 1;
    ctx.strokeStyle = css('--line-strong'); ctx.strokeRect(0.5, 0.5, w - 1, h - 1);

    // The window and the playback position, as elements over the bar.
    const box = $('tl-win'), marker = $('tl-head');
    // Anchored by its right edge, so a window too short to grab grows to the
    // left, never past the bar's end.
    box.style.right = `${100 - (X(b) / w) * 100}%`;
    box.style.width = `${((X(b) - X(a)) / w) * 100}%`;
    const windowText = `${ago(now - a)} ago to ${win.end == null ? 'now' : `${ago(now - b)} ago`}`;
    for (const id of ['tl-win', 'tl-from', 'tl-to']) setAttr($(id), 'aria-valuetext', windowText);
    marker.style.left = `${(X(head) / w) * 100}%`;
    setAttr(marker, 'aria-valuetext', headText());
    setAttr(marker, 'data-at', String(Math.round(head)));

    const tip = $('tl-tipbox');
    let best: [number, Mark, number] | null = null;
    if (hoverX != null && !dragging) {
      for (const k of keys) if (Math.abs(k[0] - hoverX) < 7 && (!best || Math.abs(k[0] - hoverX) < Math.abs(best[0] - hoverX))) best = k;
    }
    const onHead = hoverX != null && Math.abs(X(head) - hoverX) < 6;
    const onVoid = !best && !onHead && hoverX != null && hoverX < held - 2;
    tip.hidden = !best && !onHead && !onVoid;
    if (onVoid) {
      tip.textContent = `No data before ${clockAt(start)}, when the engine started: nothing to show or move to there.`;
      tip.style.left = `${Math.max(220, Math.min(w - 220, hoverX!))}px`;
    } else if (onHead) {
      tip.textContent = headText();
      tip.style.left = `${Math.max(180, Math.min(w - 180, X(head)))}px`;
    } else if (best) {
      tip.textContent = markText(best[1].what);
      tip.style.left = `${Math.max(140, Math.min(w - 140, best[0]))}px`;
    }

    setReadout(unreachable ? UNREACHABLE : isLive ? '' : `${ago(engineNow() - head)} behind live`);
    // Five-minute marks back from the bar's right edge, which is now while
    // live and the moment playback left live otherwise.
    const end = axisStop, reallyNow = engineNow();
    const label = (t: number) => (reallyNow - t < 1000 ? 'now' : `${ago(reallyNow - t)} ago`);
    const span = (text: string, left: string, cls?: string) => {
      const element = document.createElement('span');
      if (cls) element.className = cls;
      element.style.left = left;
      element.textContent = text;
      return element;
    };
    const labels = [span(label(axisStart), '0', 'edge')];
    for (let back = 5 * 60000; back < AXIS; back += 5 * 60000) {
      const p = ((AXIS - back) / AXIS) * 100;
      if ((p / 100) * w < 90 || ((100 - p) / 100) * w < 70) continue;
      labels.push(span(label(end - back), `${p}%`));
    }
    labels.push(span(label(end), '100%', 'end'));
    const tlAxis = $('tl-axis');
    const texts = labels.map((element) => `${element.className}|${element.style.left}|${element.textContent}`).join('\n');
    if (tlAxis.dataset.drawn !== texts) { tlAxis.replaceChildren(...labels); tlAxis.dataset.drawn = texts; }
    drawn = { end: axisStop, head, w, fading };
  }

  // Moving the window: by its middle, or one end by a handle. The window
  // never gets shorter than MIN_SPAN.
  function moveWindow(part: 'move' | 'from' | 'to', from: number, to: number) {
    const [start, now] = range();
    if (part === 'move') setWindow(Math.min(now, Math.max(start + (to - from), to)), to - from);
    else if (part === 'from') setWindow(to, to - Math.max(start, Math.min(from, to - MIN_SPAN)));
    else setWindow(Math.max(from + MIN_SPAN, Math.min(to, now)), Math.max(from + MIN_SPAN, Math.min(to, now)) - from);
  }

  // After the window moved: live it stays live only while it ends at now; a
  // window moved into the past pauses at its start, and a paused playback
  // position left outside it comes to its nearer edge.
  function settleWindow() {
    const [a, b] = windowSpan();
    if (mode === 'live' && win.end != null) { setMode('paused'); void seek(a); } else if (mode !== 'live' && (head < a || head > b)) void seek(Math.max(a, Math.min(b, head)));
  }

  function startReplay() { setMode('replay'); queue = []; replayTo = head; void fetchReplay(); }
  function togglePlay() { if (mode === 'paused') startReplay(); else setMode('paused'); }

  let dragging: { part: 'move' | 'from' | 'to' | 'head'; t0: number; x0: number; a: number; b: number; moved: boolean } | null = null;
  function wireTimeline() {
    const track = $('tl-track'), box = $('tl-win'), marker = $('tl-head');
    const xAt = (e: PointerEvent) => { const r = canvas.getBoundingClientRect(); return Math.max(0, Math.min(r.width, e.clientX - r.left)); };
    const tAt = (e: PointerEvent) => tOf(xAt(e), canvas.clientWidth);

    // A drag of the window, a handle or the playback position. A press on
    // the window that doesn't move goes to that moment.
    const begin = (part: 'move' | 'from' | 'to' | 'head') => (e: PointerEvent) => {
      e.preventDefault(); e.stopPropagation();
      const [a, b] = windowSpan();
      dragging = { part, t0: tAt(e), x0: e.clientX, a, b, moved: false };
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      box.classList.add('moving');
    };
    const follow = (e: PointerEvent) => {
      hoverX = xAt(e); tlDirty = true;
      if (!dragging) return;
      if (!dragging.moved && Math.abs(e.clientX - dragging.x0) < 4) return;
      dragging.moved = true;
      const dt = tAt(e) - dragging.t0, [start, now] = range();
      if (dragging.part === 'head') {
        // As if Pause were pressed, wherever it is let go; the window follows
        // it.
        if (mode !== 'paused') setMode('paused');
        const t = tAt(e);
        containPlayhead(t);
        void seek(t);
      } else if (dragging.part === 'move') {
        const shift = Math.max(start - dragging.a, Math.min(now - dragging.b, dt));
        moveWindow('move', dragging.a + shift, dragging.b + shift);
      } else if (dragging.part === 'from') moveWindow('from', dragging.a + dt, dragging.b);
      else moveWindow('to', dragging.a, dragging.b + dt);
    };
    const end = (e: PointerEvent) => {
      if (!dragging) return;
      const { part, moved } = dragging;
      dragging = null;
      box.classList.remove('moving');
      if (part === 'move' && !moved) { setMode('paused'); void seek(tAt(e)); } else if (part !== 'head') settleWindow();
    };
    box.addEventListener('pointerdown', begin('move'));
    $('tl-from').addEventListener('pointerdown', begin('from'));
    $('tl-to').addEventListener('pointerdown', begin('to'));
    marker.addEventListener('pointerdown', begin('head'));
    for (const el of [box, $('tl-from'), $('tl-to'), marker]) {
      el.addEventListener('pointermove', follow);
      el.addEventListener('pointerup', end);
      el.addEventListener('pointercancel', end);
    }
    // A press on the bar outside the window takes the window there, and the
    // playback position with it.
    canvas.addEventListener('pointerdown', (e) => {
      const [a, b] = windowSpan(), t = tAt(e);
      moveWindow('move', t - (b - a) / 2, t + (b - a) / 2);
      setMode('paused');
      void seek(t);
    });
    track.addEventListener('pointermove', (e) => { hoverX = xAt(e); tlDirty = true; });
    track.addEventListener('pointerleave', () => { hoverX = null; tlDirty = true; });

    const windowKeys = (part: 'from' | 'to') => (e: KeyboardEvent) => {
      const [a, b] = windowSpan(), step = (b - a) / 10;
      const d = e.key === 'ArrowLeft' ? -step : e.key === 'ArrowRight' ? step : 0;
      if (!d) return;
      e.preventDefault(); e.stopPropagation();
      if (part === 'from') moveWindow('from', a + d, b);
      else moveWindow('to', a, b + d);
      settleWindow();
    };
    $('tl-from').addEventListener('keydown', windowKeys('from'));
    $('tl-to').addEventListener('keydown', windowKeys('to'));
    box.addEventListener('keydown', (e) => {
      if (e.target !== box) return;
      const candidates = marks.filter((m) => e.shiftKey || m.key);
      if (e.key === 'ArrowLeft') {
        const p = candidates.filter((m) => m.at_ms < head - 1).pop();
        if (p) { setMode('paused'); containPlayhead(p.at_ms); void seek(p.at_ms); }
      } else if (e.key === 'ArrowRight') {
        const n = candidates.find((m) => m.at_ms > head + 1);
        if (n) { setMode('paused'); containPlayhead(n.at_ms); void seek(n.at_ms); }
      } else if (e.key === 'End') goLive();
      else if (e.key === ' ') togglePlay();
      else return;
      e.preventDefault();
    });
    $('tl-mode').addEventListener('change', (event) => {
      const value = (event.target as HTMLSelectElement).value;
      if (value === 'live') goLive();
      else if (value === 'replay') startReplay();
      else setMode('paused');
    });
  }

  // ---- start ----
  wireTimeline();
  addEventListener('resize', () => { tlDirty = true; });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) { clearEffects(); queue = queue.slice(-1); } else { lastTick = performance.now(); tlDirty = true; if (mode === 'live') queue = queue.slice(-1); }
  });
  function start() {
    stopped = false;
    lastTick = performance.now();
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class'] });
    refreshTimer = window.setInterval(() => {
      if (document.hidden) return;
      if (mode !== 'live') tlDirty = true;
      drawMachineAge();
    }, 1000);
    connect();
    animationFrame = requestAnimationFrame(frameLoop);
  }
  window.addEventListener('pagehide', () => {
    stopped = true;
    source?.close();
    cancelAnimationFrame(animationFrame);
    clearInterval(refreshTimer);
    clearEffects();
    for (const controller of pendingFetches) controller.abort();
    pendingFetches.clear();
    themeObserver.disconnect();
    queue = [];
  });
  window.addEventListener('pageshow', (event) => { if (event.persisted && stopped) start(); });
  start();
})();

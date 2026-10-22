/** The POS session timeline: what happened on this terminal, in order, sent
 * to the store's logs (`POST {api}/logs`, `http::pos_logs`) so a problem
 * can be traced through the whole session - coverage lost and found, the
 * app hidden and shown, the live stream dropping, orders created,
 * backgrounded, brought back and finished.
 *
 * Only when the store opted in to client logs (`data-client-logging` on
 * the root); otherwise every call here does nothing. Events are queued in
 * sessionStorage, so a reload, the browser discarding the tab or an hour
 * without coverage loses nothing: they are sent in batches every few
 * seconds while online, straight away when the page is hidden (a beacon,
 * which survives the page going away), and as soon as the tablet is back
 * online. A 429 pauses sending until its Retry-After; a 403 (Diagnostics
 * turned off meanwhile) stops recording. The queue keeps at most
 * MAX_QUEUED events, dropping the oldest and saying how many were lost.
 *
 * Never recorded: a note's text, addresses, keys. */

export type Level = 'info' | 'warn' | 'error';
export type Detail = Record<string, string | number | boolean | null | undefined>;
type Event = { seq: number; t: number; level: Level; kind: string; order_id?: string; detail?: Detail };
type Stored = { session: string; seq: number; queue: Event[]; dropped: number };

const STORAGE_KEY = 'monokulo-pos-timeline';
const MAX_QUEUED = 500;
const BATCH = 50;
const SEND_EVERY_MS = 5000;

export interface Timeline {
  readonly enabled: boolean;
  readonly session: string;
  record(kind: string, detail?: Detail & { order_id?: string }, level?: Level): void;
  /** Sends what is queued now (also runs by itself). */
  flush(): Promise<void>;
}

const disabled: Timeline = { enabled: false, session: '', record() {}, async flush() {} };

function load(): Stored {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw) {
      const stored = JSON.parse(raw) as Stored;
      if (stored && typeof stored.session === 'string' && Array.isArray(stored.queue)) return stored;
    }
  } catch { /* Storage blocked or corrupt: start a new session. */ }
  return { session: crypto.randomUUID(), seq: 0, queue: [], dropped: 0 };
}

/** Timelines are per terminal page; `endpoint` is the POS's own
 * `/dashboard/stores/{id}/pos/logs`. */
export function createTimeline(enabled: boolean, endpoint: string): Timeline {
  if (!enabled) return disabled;
  const state = load();
  let stopped = false;
  let sending = false;
  let pausedUntil = 0;
  /** The last event of the batch a fetch is carrying now, so a beacon
   * sent meanwhile (the page hiding) doesn't carry it again. */
  let inFlightUpTo = 0;

  function save() {
    try { sessionStorage.setItem(STORAGE_KEY, JSON.stringify(state)); }
    catch { /* Full or blocked: the queue still lives in memory. */ }
  }

  function record(kind: string, detail: Detail & { order_id?: string } = {}, level: Level = 'info') {
    if (stopped) return;
    const { order_id, ...rest } = detail;
    const clean: Detail = {};
    for (const [key, value] of Object.entries(rest)) if (value !== undefined && value !== null) clean[key] = value;
    state.seq += 1;
    state.queue.push({ seq: state.seq, t: Date.now(), level, kind, order_id: order_id || undefined, detail: clean });
    if (state.queue.length > MAX_QUEUED) {
      const excess = state.queue.length - MAX_QUEUED;
      state.queue.splice(0, excess);
      state.dropped += excess;
    }
    save();
  }

  /** The next batch after `after`, noting any events dropped from a full
   * queue first. */
  function nextBatch(after = 0): Event[] {
    if (state.dropped) {
      const dropped = state.dropped;
      state.dropped = 0;
      record('timeline.dropped', { events: dropped }, 'warn');
    }
    return state.queue.filter(e => e.seq > after).slice(0, BATCH);
  }

  function sent(batch: Event[]) {
    const done = new Set(batch.map(e => e.seq));
    state.queue = state.queue.filter(e => !done.has(e.seq));
    save();
  }

  async function flush() {
    if (stopped || sending || !state.queue.length || !navigator.onLine || Date.now() < pausedUntil) return;
    sending = true;
    const batch = nextBatch();
    inFlightUpTo = batch[batch.length - 1]?.seq ?? 0;
    let accepted = false;
    try {
      const response = await fetch(endpoint, {
        method: 'POST', credentials: 'same-origin', keepalive: true,
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ session: state.session, events: batch }),
      });
      if (response.ok) { sent(batch); accepted = true; }
      else if (response.status === 429) {
        const seconds = Number(response.headers.get('Retry-After'));
        pausedUntil = Date.now() + (seconds > 0 ? seconds : 60) * 1000;
      } else if (response.status === 403 || response.status === 404) {
        // Diagnostics turned off, or the store is gone: stop and forget.
        stopped = true;
        state.queue = [];
        try { sessionStorage.removeItem(STORAGE_KEY); } catch { /* ignore */ }
      } else if (response.status === 400 || response.status === 413) {
        sent(batch); // Never accepted; don't retry it forever.
      }
    } catch { /* Offline or the server unreachable: kept for the next try. */ }
    finally { sending = false; inFlightUpTo = 0; }
    // More than one batch waiting (after a long time offline): keep going.
    if (accepted && state.queue.length) void flush();
  }

  /** As the page is hidden or goes away: a beacon outlives the page. It
   * carries what no fetch is carrying already. (Should that fetch then
   * fail, its events are sent again later; the timeline page ignores an
   * event it has already seen, by `seq`.) */
  function beacon() {
    if (stopped || !state.queue.length || !navigator.onLine || Date.now() < pausedUntil || !navigator.sendBeacon) return;
    const batch = nextBatch(inFlightUpTo);
    if (!batch.length) return;
    const body = new Blob([JSON.stringify({ session: state.session, events: batch })], { type: 'application/json' });
    try { if (navigator.sendBeacon(endpoint, body)) sent(batch); }
    catch { /* Kept for the next try. */ }
  }

  window.setInterval(() => void flush(), SEND_EVERY_MS);
  window.addEventListener('online', () => void flush());
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') beacon(); });
  window.addEventListener('pagehide', beacon);

  return { enabled: true, session: state.session, record, flush };
}

/** Records how the page and the network come and go: offline and back
 * (with how long it was out), hidden and shown (how long it was away),
 * frozen and resumed by the browser, restored from the back/forward cache,
 * and script errors. */
export function watchPage(timeline: Timeline) {
  if (!timeline.enabled) return;
  const navigation = performance.getEntriesByType?.('navigation')[0] as PerformanceNavigationTiming | undefined;
  timeline.record('pos.opened', {
    navigation: navigation?.type, online: navigator.onLine, visibility: document.visibilityState,
    width: window.innerWidth, height: window.innerHeight, agent: navigator.userAgent,
  });
  let offlineAt = navigator.onLine ? 0 : Date.now();
  let hiddenAt = document.visibilityState === 'hidden' ? Date.now() : 0;
  window.addEventListener('offline', () => { offlineAt = Date.now(); timeline.record('network.offline', {}, 'warn'); });
  window.addEventListener('online', () => {
    timeline.record('network.online', { offline_ms: offlineAt ? Date.now() - offlineAt : undefined });
    offlineAt = 0;
  });
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') { hiddenAt = Date.now(); timeline.record('page.hidden'); }
    else { timeline.record('page.visible', { hidden_ms: hiddenAt ? Date.now() - hiddenAt : undefined }); hiddenAt = 0; }
  });
  window.addEventListener('pagehide', event => timeline.record('page.pagehide', { persisted: event.persisted }));
  window.addEventListener('pageshow', event => { if (event.persisted) timeline.record('page.restored'); });
  document.addEventListener('freeze', () => timeline.record('page.frozen'));
  document.addEventListener('resume', () => timeline.record('page.resumed'));
  window.addEventListener('error', event => timeline.record('script.error', {
    message: event.message, where: event.filename ? `${event.filename}:${event.lineno}:${event.colno}` : undefined,
  }, 'error'));
  window.addEventListener('unhandledrejection', event => {
    const reason = event.reason as { message?: string } | undefined;
    timeline.record('script.rejection', { message: reason?.message || String(event.reason) }, 'error');
  });
}

/** An API path with its ids replaced, so requests group by route:
 * `/dashboard/stores/c1/pos/orders/order_8f42/cancel` → `/pos/orders/{id}/cancel`. */
export function routeOf(url: string): string {
  const path = url.split('?')[0];
  const pos = path.replace(/^\/dashboard\/stores\/[^/]+\/pos/, '/pos');
  return pos.replace(/\/pay\/[^/]+\//, '/pay/{pk}/').replace(/\/orders\/[^/]+/, '/orders/{id}');
}

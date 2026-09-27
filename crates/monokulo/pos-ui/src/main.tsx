import { createMemo, createSignal, For, onCleanup, Show } from 'solid-js';
import { render } from '@solidjs/web';
import { StatusBadge, StatusIcon, StatusSymbols, stateOf, statusName } from './status';
import { addressFromQr, decodeImageFile, looksLikeAddress, scanCamera } from './refund';
import { applyTheme, type Theme } from './theme';
import './pos.css';

type Order = {
  order_id: string; merchant_order_id: string | null; address: string;
  xmr_amount: string; amount: string; currency: string; status: string;
  confirmations: number; confirmations_required: number; error: string | null;
  updated_at?: number;
  backgrounded: boolean; cancelled_at: number | null; created_at: number; expires_at: number;
  received_xmr?: string; remaining_xmr?: string;
  refund_address?: string | null; qr_svg?: string;
};
type StatusEvent = Pick<Order, 'order_id' | 'status' | 'confirmations' | 'confirmations_required' | 'error' | 'updated_at' | 'received_xmr' | 'remaining_xmr'> & { is_terminal: boolean };
type Config = { connectionId: string; publicKey: string; currency: string; decimals: number; storeName: string };

const root = document.getElementById('pos-root');
if (!root) throw new Error('POS root missing');
const config: Config = {
  connectionId: root.dataset.connectionId || '', publicKey: root.dataset.publicKey || '',
  currency: root.dataset.currency || 'AUD', decimals: Number(root.dataset.decimals || '2'),
  storeName: root.dataset.storeName || 'Store',
};
const api = `/dashboard/stores/${encodeURIComponent(config.connectionId)}/pos`;
const terminal = (o: Order) => Boolean(o.cancelled_at) || ['paid', 'overpaid', 'expired'].includes(o.status);

/** `#8f42…a91c` - the order ID's distinctive part, without its `order_` prefix. */
function shortId(id: string): string {
  const core = id.replace(/^order_/, '');
  return core.length <= 10 ? `#${core}` : `#${core.slice(0, 4)}…${core.slice(-4)}`;
}
const label = (o: Order) => o.merchant_order_id || shortId(o.order_id);
/** An exact XMR amount without trailing zeros (`0.052431000000` → `0.052431`). */
const trimXmr = (value: string) => value.includes('.') ? value.replace(/0+$/, '').replace(/\.$/, '') : value;
const plainAmount = (digits: string) => {
  const padded = digits.padStart(config.decimals + 1, '0');
  return config.decimals ? `${padded.slice(0, -config.decimals) || '0'}.${padded.slice(-config.decimals)}` : padded;
};
const displayAmount = (digits: string) => {
  const [whole, fraction] = plainAmount(digits).split('.');
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return fraction === undefined ? grouped : `${grouped}.${fraction}`;
};
/** "28m", "1h 5m", "2d 4h" until `unix`; "less than a minute" when close. */
function durationUntil(unix: number, now: number): string {
  const seconds = Math.max(0, unix - now);
  const days = Math.floor(seconds / 86400), hours = Math.floor((seconds % 86400) / 3600), minutes = Math.floor((seconds % 3600) / 60);
  if (days) return hours ? `${days}d ${hours}h` : `${days}d`;
  if (hours) return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
  return minutes ? `${minutes}m` : 'less than a minute';
}
const clock = (unix: number) => new Date(unix * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
/** A long address shown on one line: start … middle … end. */
function shortAddress(address: string): string {
  if (address.length < 40) return address;
  const middle = Math.floor(address.length / 2);
  return `${address.slice(0, 5)}…${address.slice(middle - 9, middle + 9)}…${address.slice(-4)}`;
}

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new Error(body.error || `Request failed (${response.status})`);
  }
  return response.status === 204 ? undefined as T : response.json();
}
const post = (url: string, value?: unknown) => json<void>(url, {
  method: 'POST', headers: value === undefined ? undefined : { 'content-type': 'application/json' },
  body: value === undefined ? undefined : JSON.stringify(value),
});

const [now, setNow] = createSignal(Math.floor(Date.now() / 1000));
window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 15000);

/** Moves the site's own status indicator and theme toggle (rendered by
 * the server into #pos-site-controls) into the top bar, and makes the
 * toggle apply a theme in place instead of reloading the terminal. */
function adoptSiteControls(slot: HTMLElement) {
  const controls = document.getElementById('pos-site-controls');
  if (!controls) return;
  const toggle = controls.querySelector<HTMLElement>('.theme-toggle');
  const form = controls.querySelector<HTMLFormElement>('.nav-theme-form');
  form?.addEventListener('submit', event => {
    const button = (event as SubmitEvent).submitter as HTMLButtonElement | null;
    const theme = button?.value as Theme | undefined;
    if (!theme || !toggle) return;
    event.preventDefault();
    toggle.className = `theme-toggle theme-toggle-${theme}`;
    toggle.querySelectorAll('button[name="theme"]').forEach(b => b.setAttribute('aria-pressed', String((b as HTMLButtonElement).value === theme)));
    void applyTheme(theme);
  });
  slot.append(...Array.from(controls.children));
  controls.remove();
}

/** Sketch 2's payment card: what the customer pays, where, and where a
 * refund would go. */
function PaymentCard(props: { order: Order }) {
  // A memo, so an order refresh carrying the same QR doesn't rebuild it.
  const qr = createMemo(() => props.order.qr_svg);
  const [copied, setCopied] = createSignal(false);
  const [refund, setRefund] = createSignal(props.order.refund_address || '');
  const [saved, setSaved] = createSignal(props.order.refund_address || '');
  const [refundState, setRefundState] = createSignal<'idle' | 'saving' | 'saved' | 'invalid'>(props.order.refund_address ? 'saved' : 'idle');
  const [refundMessage, setRefundMessage] = createSignal('');
  const [scanning, setScanning] = createSignal(false);
  let video: HTMLVideoElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let stopScan: (() => void) | null = null;
  let saveTimer: number | undefined;
  onCleanup(() => { stopScan?.(); window.clearTimeout(saveTimer); });

  // Awaiting: the customer still has to pay (all of it, or the rest).
  // Otherwise the payment has been seen and the card says what arrived
  // instead of inviting a second payment.
  const awaiting = () => ['pending', 'partial'].includes(props.order.status) && !props.order.cancelled_at;
  const partial = () => props.order.status === 'partial';
  const detail = () => {
    const o = props.order;
    switch (stateOf(o)) {
      case 'double-spend': return o.error || 'Double spend detected. Do not treat this payment as paid.';
      case 'unconfirmed': return 'Payment seen. Waiting for its first confirmation.';
      case 'confirming': return `Payment seen · ${o.confirmations} of ${o.confirmations_required} confirmations`;
      case 'partial': return `${trimXmr(o.received_xmr || '0')} of ${trimXmr(o.xmr_amount)} XMR received`;
      default: return '';
    }
  };

  async function copy() {
    try { await navigator.clipboard.writeText(props.order.address); setCopied(true); window.setTimeout(() => setCopied(false), 2000); }
    catch { /* Without clipboard access the address stays selectable. */ }
  }
  async function save(value: string) {
    if (!looksLikeAddress(value) || value === saved()) return;
    setRefundState('saving'); setRefundMessage('');
    try {
      const response = await fetch(`/pay/${encodeURIComponent(config.publicKey)}/orders/${encodeURIComponent(props.order.order_id)}/refund-address`, {
        method: 'POST', headers: { accept: 'application/json', 'content-type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams({ refund_address: value }),
      });
      const body = await response.json().catch(() => ({}));
      if (refund().trim() !== value) return;
      if (response.ok) { setSaved(value); setRefundState('saved'); }
      else { setRefundState('invalid'); setRefundMessage(body.error || 'That address was not accepted.'); }
    } catch {
      if (refund().trim() === value) { setRefundState('idle'); setRefundMessage('Could not save. Check the connection and try again.'); }
    }
  }
  function onRefundInput(value: string) {
    setRefund(value); setRefundMessage('');
    const trimmed = value.trim();
    window.clearTimeout(saveTimer);
    if (!trimmed) { setRefundState('idle'); return; }
    if (trimmed === saved()) { setRefundState('saved'); return; }
    if (!looksLikeAddress(trimmed)) { setRefundState(trimmed.length >= 95 ? 'invalid' : 'idle'); return; }
    setRefundState('idle');
    saveTimer = window.setTimeout(() => void save(trimmed), 500);
  }
  function useQr(payload: string | null) {
    const address = payload ? addressFromQr(payload) : null;
    if (!address) { setRefundMessage(payload ? 'That QR code does not contain a Monero address.' : 'No QR code found in that image.'); return; }
    setRefund(address); void save(address);
  }
  async function scan() {
    if (scanning()) { stopScan?.(); return; }
    setRefundMessage('');
    if (!video) return;
    const session = scanCamera(video);
    stopScan = session.stop; setScanning(true);
    try { const payload = await session.result; if (payload) useQr(payload); }
    catch { setRefundMessage('Camera unavailable. Choose a QR image instead.'); }
    finally { setScanning(false); stopScan = null; }
  }
  async function chooseImage(file: File | undefined) {
    if (!file) return;
    setRefundMessage('');
    try { useQr(await decodeImageFile(file)); }
    catch { setRefundMessage('Could not read that image. Choose another file.'); }
    if (fileInput) fileInput.value = '';
  }

  return <section class="pos-pay-card" aria-label="Payment details">
    <Show when={awaiting()}><p class="pos-expiry">Send payment within {durationUntil(props.order.expires_at, now())}</p></Show>
    <Show when={detail()}><p class={`pos-pay-detail state-${stateOf(props.order)}`}>{detail()}</p></Show>
    <Show when={awaiting()} fallback={<>
      <p class="pos-pay-caption">Received</p>
      <p class="pos-pay-xmr">{trimXmr(props.order.received_xmr || props.order.xmr_amount)} <span>XMR</span></p>
      <Show when={props.order.currency !== 'XMR'}><p class="pos-pay-fiat">for {props.order.amount} {props.order.currency}</p></Show>
    </>}>
      <p class="pos-pay-caption">{partial() ? 'Send the remaining amount' : 'Send exactly this amount'}</p>
      <p class="pos-pay-xmr">{trimXmr(partial() ? props.order.remaining_xmr || props.order.xmr_amount : props.order.xmr_amount)} <span>XMR</span></p>
      <Show when={props.order.currency !== 'XMR' && !partial()}><p class="pos-pay-fiat">≈ {props.order.amount} {props.order.currency}</p></Show>
      <Show when={qr()}><div class="pos-qr" innerHTML={qr()}/></Show>
      <p class="pos-quiet-label">Payment address</p>
      <div class="pos-address">
        <code title={props.order.address}>{shortAddress(props.order.address)}</code>
        <button type="button" onClick={() => void copy()} aria-label="Copy payment address">{copied() ? 'Copied' : 'Copy'}</button>
      </div>
    </Show>
    <hr/>
    <label class="pos-field-label" for="pos-refund">Refund address <span>(optional)</span></label>
    <div class={`pos-refund state-${refundState()}`}>
      <input id="pos-refund" type="text" autocomplete="off" spellcheck={false} placeholder="Your Monero refund address"
        value={refund()} onInput={event => onRefundInput(event.currentTarget.value)} onBlur={() => void save(refund().trim())}
        aria-invalid={refundState() === 'invalid' ? 'true' : 'false'} aria-describedby="pos-refund-note"/>
      <span class="pos-refund-state" role="status" aria-label={refundState() === 'saving' ? 'Saving refund address' : refundState() === 'saved' ? 'Refund address saved' : ''}>
        <Show when={refundState() === 'saved'}><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 5 5L19 7"/></svg></Show>
        <Show when={refundState() === 'saving'}><span class="pos-spinner pos-spinner-small"/></Show>
      </span>
    </div>
    <div class="pos-refund-tools">
      <Show when={navigator.mediaDevices?.getUserMedia}>
        <button type="button" onClick={() => void scan()}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M3 8V3h5M16 3h5v5M21 16v5h-5M8 21H3v-5M7 7h3v3H7zM14 7h3v3h-3zM7 14h3v3H7zM14 14h3v3h-3z"/></svg>
          {scanning() ? 'Stop camera' : 'Scan refund QR'}
        </button>
      </Show>
      <button type="button" onClick={() => fileInput?.click()}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="1.5"/><circle cx="9" cy="10" r="1.6"/><path d="m3 17 5-5 4 4 3-3 6 6"/></svg>
        Choose QR image
      </button>
      <input ref={el => { fileInput = el; }} type="file" accept="image/*" hidden onChange={event => void chooseImage(event.currentTarget.files?.[0])}/>
    </div>
    <video ref={el => { video = el; }} class="pos-camera" autoplay playsinline muted hidden={!scanning()}/>
    <Show when={refundMessage()}><p class="pos-refund-message" role="alert">{refundMessage()}</p></Show>
    <p class="pos-note" id="pos-refund-note">Recorded for the merchant if a refund is needed. Refunds are not sent automatically.</p>
  </section>;
}

/** A finished order (paid, overpaid, expired, cancelled): the outcome,
 * never a live payment prompt. */
function Outcome(props: { order: Order }) {
  const message = () => {
    const o = props.order;
    if (o.cancelled_at) return o.status === 'pending' ? 'This order was cancelled. If money still arrives at its address, it will show in the order for review.' : 'Payment activity arrived after this order was cancelled. Review it in the order details.';
    if (o.error) return o.error;
    if (o.status === 'paid') return 'Payment received and confirmed.';
    if (o.status === 'expired') return 'This order expired before it was paid.';
    return statusName[stateOf(o)] || o.status;
  };
  return <section class={`pos-outcome state-${stateOf(props.order)}`}>
    <StatusIcon order={props.order}/>
    <p class="pos-outcome-title">{statusName[stateOf(props.order)]}</p>
    <p>{message()}</p>
    <p class="pos-outcome-amount">{trimXmr(props.order.xmr_amount)} XMR<Show when={props.order.currency !== 'XMR'}> · {props.order.amount} {props.order.currency}</Show></p>
  </section>;
}

function App() {
  const [orders, setOrders] = createSignal<Order[]>([]);
  const [screen, setScreen] = createSignal<'keypad' | 'payment' | 'list'>('keypad');
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [digits, setDigits] = createSignal('0');
  const [reference, setReference] = createSignal('');
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [offline, setOffline] = createSignal(false);
  const [search, setSearch] = createSignal('');
  const [searchOrders, setSearchOrders] = createSignal<Order[]>([]);
  const [searchTotal, setSearchTotal] = createSignal(0);
  const [searchOffset, setSearchOffset] = createSignal(0);
  const [searching, setSearching] = createSignal(false);
  const [tab, setTab] = createSignal<'active' | 'finished'>('active');
  const [total, setTotal] = createSignal(0);
  const [nextOffset, setNextOffset] = createSignal(0);
  const [listScroll, setListScroll] = createSignal(0);
  const active = createMemo(() => orders().find(o => o.order_id === activeId()) || null);
  const background = createMemo(() => orders().filter(o => !terminal(o) && o.order_id !== activeId()));
  const activeCount = createMemo(() => orders().filter(o => !terminal(o)).length);
  const finishedCount = createMemo(() => orders().filter(terminal).length);
  const visible = createMemo(() => (search().trim() ? searchOrders() : orders())
    .filter(o => (tab() === 'active') !== terminal(o)));
  const amountText = createMemo(() => displayAmount(digits()));
  let stream: EventSource | null = null;
  let lostTimer: number | undefined;
  let searchTimer: number | undefined;
  let searchGeneration = 0;
  let listElement: HTMLElement | undefined;
  let pendingRequest: { amount: string; reference: string; key: string } | null = null;

  function merge(previous: Order, next: Partial<Order> & { updated_at?: number }): Order {
    if (previous.updated_at !== undefined && next.updated_at !== undefined && next.updated_at < previous.updated_at) return previous;
    return { ...previous, ...next, qr_svg: next.qr_svg ?? previous.qr_svg, refund_address: next.refund_address !== undefined ? next.refund_address : previous.refund_address };
  }
  function upsert(order: Order) {
    setOrders(previous => previous.some(o => o.order_id === order.order_id)
      ? previous.map(o => o.order_id === order.order_id ? merge(o, order) : o) : [order, ...previous]);
    setSearchOrders(previous => previous.map(o => o.order_id === order.order_id ? merge(o, order) : o));
  }
  async function refreshSearch(append = false) {
    const term = search().trim();
    if (!term) return;
    const generation = searchGeneration;
    setSearching(true);
    const offset = append ? searchOffset() : 0;
    try {
      const data = await json<{ orders: Order[]; total: number }>(`${api}/orders?offset=${offset}&limit=40&search=${encodeURIComponent(term)}`);
      if (generation !== searchGeneration) return;
      setSearchTotal(data.total);
      setSearchOffset(offset + data.orders.length);
      setSearchOrders(previous => append ? [...previous, ...data.orders.filter(o => !previous.some(p => p.order_id === o.order_id))] : data.orders);
      setError('');
      queueMicrotask(openStream);
    } catch (e) { if (generation === searchGeneration) setError((e as Error).message); }
    finally { if (generation === searchGeneration) setSearching(false); }
  }
  function onSearch(value: string) {
    searchGeneration++;
    setSearch(value);
    window.clearTimeout(searchTimer);
    if (value.trim()) {
      setSearchOrders([]); setSearchOffset(0); setSearchTotal(0); setSearching(true); setError('');
      searchTimer = window.setTimeout(() => void refreshSearch(), 200);
    } else { setSearchOrders([]); setSearchOffset(0); setSearchTotal(0); setSearching(false); setError(''); queueMicrotask(openStream); }
  }
  async function refresh(append = false) {
    try {
      const offset = append ? nextOffset() : 0;
      const data = await json<{ orders: Order[]; total: number }>(`${api}/orders?offset=${offset}&limit=40`);
      setTotal(data.total);
      setNextOffset(offset + data.orders.length);
      setOrders(previous => append ? [...previous, ...data.orders.filter(o => !previous.some(p => p.order_id === o.order_id))]
        : data.orders.map(o => { const known = previous.find(p => p.order_id === o.order_id); return known ? merge(known, o) : o; }));
      if (!append && !activeId()) {
        const foreground = data.orders.find(o => !o.backgrounded && !terminal(o));
        if (foreground) { setActiveId(foreground.order_id); setScreen('payment'); void loadOrder(foreground.order_id).catch(() => {}); }
      }
      setError('');
      queueMicrotask(openStream);
    } catch (e) { setError((e as Error).message); }
    finally { setLoading(false); }
  }
  async function loadOrder(id: string) {
    const order = await json<Order>(`${api}/orders/${encodeURIComponent(id)}`);
    upsert(order);
    return order;
  }
  function watchedIds() {
    const foreground = active() && !terminal(active()!) ? [active()!.order_id] : [];
    const onScreen = screen() === 'list' ? visible().filter(o => !terminal(o)).map(o => o.order_id) : background().map(o => o.order_id);
    const otherActive = orders().filter(o => !terminal(o)).map(o => o.order_id);
    const cancelled = orders().filter(o => o.cancelled_at && o.status === 'pending').slice(0, 8).map(o => o.order_id);
    return [...new Set([...foreground, ...onScreen, ...otherActive, ...cancelled])].slice(0, 32);
  }
  function openStream() {
    stream?.close(); stream = null;
    const ids = watchedIds();
    if (!ids.length) { window.clearTimeout(lostTimer); lostTimer = undefined; setOffline(false); return; }
    const source = new EventSource(`${api}/events?orders=${ids.map(encodeURIComponent).join(',')}`);
    stream = source;
    // The stream sends every watched order's status when it connects, so
    // opening it needs no per-order reads of its own (the engine
    // rate-limits each store).
    source.addEventListener('open', () => { if (stream !== source) return; window.clearTimeout(lostTimer); lostTimer = undefined; setOffline(false); });
    source.addEventListener('status', event => {
      if (stream !== source) return;
      try {
        const update = JSON.parse((event as MessageEvent).data) as StatusEvent;
        setOrders(previous => previous.map(o => o.order_id === update.order_id ? merge(o, update) : o));
        setSearchOrders(previous => previous.map(o => o.order_id === update.order_id ? merge(o, update) : o));
        if (update.is_terminal) queueMicrotask(openStream);
      } catch { /* A malformed event is ignored; the next snapshot reconciles. */ }
    });
    // The browser retries a dropped stream every few seconds; the counter is
    // offline once it has failed to reconnect for 6s, however many attempts
    // that took. Only a successful open clears it.
    source.addEventListener('error', () => {
      if (stream !== source || lostTimer !== undefined) return;
      lostTimer = window.setTimeout(() => setOffline(true), 6000);
    });
  }
  function resetKeypad() { setDigits('0'); setReference(''); setActiveId(null); setScreen('keypad'); setError(''); }
  function pushDigit(d: string) { setDigits(value => (value + d).slice(-(config.decimals + 9)).replace(/^0+(?=\d)/, '') || '0'); }
  function backspace() { setDigits(value => value.length > 1 ? value.slice(0, -1) : '0'); }
  async function charge() {
    if (busy() || /^0+$/.test(digits())) return;
    setBusy(true); setError('');
    const amount = plainAmount(digits());
    const note = reference().trim();
    if (!pendingRequest || pendingRequest.amount !== amount || pendingRequest.reference !== note) {
      pendingRequest = { amount, reference: note, key: crypto.randomUUID() };
    }
    try {
      const created = await json<{ order_id: string }>(`${api}/orders`, {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ amount, merchant_order_id: note || null, request_key: pendingRequest.key }),
      });
      await loadOrder(created.order_id);
      pendingRequest = null;
      setActiveId(created.order_id); setScreen('payment'); queueMicrotask(openStream);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function backgroundOrder() {
    const order = active(); if (!order || busy()) return;
    setBusy(true); setError('');
    try {
      await post(`${api}/orders/${encodeURIComponent(order.order_id)}/background`);
      upsert({ ...order, backgrounded: true }); resetKeypad(); queueMicrotask(openStream);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function cancelOrder() {
    const order = active();
    if (!order || busy() || !window.confirm(`Cancel ${label(order)}? The payment address has already been issued; any later payment will still need review.`)) return;
    setBusy(true); setError('');
    try {
      await post(`${api}/orders/${encodeURIComponent(order.order_id)}/cancel`);
      await loadOrder(order.order_id); queueMicrotask(openStream);
    } catch (e) {
      setError((e as Error).message);
      // Refused because a payment arrived meanwhile: show it.
      void loadOrder(order.order_id).catch(() => {});
    }
    finally { setBusy(false); }
  }
  async function openOrder(order: Order) {
    if (screen() === 'list' && listElement) setListScroll(listElement.scrollTop);
    setError(''); setActiveId(order.order_id); setScreen('payment');
    try { await loadOrder(order.order_id); queueMicrotask(openStream); } catch (e) { setError((e as Error).message); }
  }
  function showList() { setScreen('list'); queueMicrotask(() => { if (listElement) listElement.scrollTop = listScroll(); openStream(); }); }
  function onKey(event: KeyboardEvent) {
    if (screen() !== 'keypad') return;
    if (event.target instanceof HTMLInputElement) { if (event.key === 'Enter') void charge(); return; }
    if (/^[0-9]$/.test(event.key)) pushDigit(event.key);
    else if (event.key === 'Backspace') backspace();
    else if (event.key === 'Escape') setDigits('0');
    else if (event.key === 'Enter') void charge();
  }
  document.addEventListener('keydown', onKey);
  queueMicrotask(() => { void refresh(); });
  onCleanup(() => { document.removeEventListener('keydown', onKey); stream?.close(); window.clearTimeout(lostTimer); window.clearTimeout(searchTimer); });

  const orderLine = (o: Order) => `${o.merchant_order_id ? 'Reference · ' : ''}${shortId(o.order_id)} · created ${clock(o.created_at)}`;
  /** The card's one-line state detail (sketch 4): short, never the
   * engine's full message. */
  const cardDetail = (o: Order) => {
    if (o.cancelled_at) return o.status === 'pending' ? 'Cancelled before payment' : 'Payment after cancellation · review';
    switch (stateOf(o)) {
      case 'pending': return `Expires in ${durationUntil(o.expires_at, now())}`;
      case 'unconfirmed': return 'Payment seen, not yet confirmed';
      case 'confirming': return `${o.confirmations} of ${o.confirmations_required} confirmations`;
      case 'partial': return 'Waiting for remaining amount';
      case 'double-spend': return 'Double spend detected · do not treat as paid';
      case 'paid': return 'Settled';
      case 'overpaid': return 'Extra amount received · review';
      case 'expired': return 'Expired unpaid';
      default: return statusName[stateOf(o)] || o.status;
    }
  };

  return <><StatusSymbols/>
    <header class="pos-top">
      <Show when={screen() === 'list'} fallback={
        <a class="pos-store" href={`/dashboard/stores/${encodeURIComponent(config.connectionId)}`}>{config.storeName}</a>
      }>
        <button class="pos-back" type="button" onClick={() => { setScreen('keypad'); queueMicrotask(openStream); }} aria-label="Back to POS">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 5-7 7 7 7"/></svg>
        </button>
        <strong>POS</strong>
      </Show>
      <span class="pos-top-end">
        <Show when={screen() !== 'list' && orders().length > 0}>
          <button class="pos-orders-link" type="button" onClick={showList} aria-label="All orders" title="All orders">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1" fill="currentColor"/><circle cx="4.5" cy="12" r="1" fill="currentColor"/><circle cx="4.5" cy="18" r="1" fill="currentColor"/></svg>
          </button>
        </Show>
        <span class="pos-site-controls" ref={adoptSiteControls}/>
      </span>
    </header>

    <Show when={screen() === 'keypad' && background().length > 0}>
      <section class="pos-stack" aria-label="Background orders">
        <div class="pos-stack-heading"><strong>Background orders · {background().length}</strong><button type="button" onClick={showList}>View all →</button></div>
        <div class="pos-stack-scroll" tabindex="0" aria-label="Background orders, scroll sideways" onWheel={event => { const el = event.currentTarget; if (el.scrollWidth > el.clientWidth && Math.abs(event.deltaY) > Math.abs(event.deltaX)) { el.scrollLeft += event.deltaY; event.preventDefault(); } }}>
          <For each={background()}>{order => <button type="button" class={`pos-stack-card state-${stateOf(order, offline())}`}
            title={`${label(order)} · ${statusName[stateOf(order, offline())]} · ${order.amount} ${order.currency}`}
            aria-label={`Open ${label(order)}, ${statusName[stateOf(order, offline())]}, ${order.amount} ${order.currency}`} onClick={() => void openOrder(order)}>
            <StatusIcon order={order} offline={offline()}/><span class="pos-stack-ref">{label(order)}</span><span class="pos-stack-amount">{order.amount}</span>
          </button>}</For>
        </div>
      </section>
    </Show>

    <Show when={screen() === 'keypad'}>
      <main class={`pos-keypad ${background().length ? 'has-stack' : ''}`}>
        <p class={`pos-amount len-${Math.min(4, Math.floor(amountText().length / 6))}`} aria-live="polite">
          {amountText()}<span>{config.currency}</span>
        </p>
        <div class="pos-keys">
          <For each={['1', '2', '3', '4', '5', '6', '7', '8', '9', 'C', '0', '⌫']}>{key => <button type="button"
            class={key === 'C' ? 'clear' : key === '⌫' ? 'delete' : ''} aria-label={key === 'C' ? 'Clear' : key === '⌫' ? 'Backspace' : key}
            onClick={() => key === 'C' ? setDigits('0') : key === '⌫' ? backspace() : pushDigit(key)}>
            {key === '⌫' ? <svg viewBox="0 0 28 20" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linejoin="round" aria-hidden="true"><path d="M9 2h16a1.5 1.5 0 0 1 1.5 1.5v13A1.5 1.5 0 0 1 25 18H9l-7.5-8Z"/><path d="m13 6.5 8 7m0-7-8 7" stroke-linecap="round"/></svg> : key}
          </button>}</For>
        </div>
        <div class="pos-field">
          <label class="pos-field-label" for="pos-reference">Reference <span>(optional)</span></label>
          <input id="pos-reference" class="pos-input" type="text" maxlength="120" placeholder="E.g. customer name or note" autocomplete="off"
            value={reference()} onInput={event => setReference(event.currentTarget.value)}/>
        </div>
        <Show when={error()}><p class="pos-error" role="alert">{error()}</p></Show>
        <button type="button" class="pos-primary" disabled={/^0+$/.test(digits()) || busy()} onClick={() => void charge()}>{busy() ? 'Creating order…' : 'Charge'}</button>
      </main>
    </Show>

    <Show when={screen() === 'payment' ? activeId() : null} keyed>{_id => <Show when={active()} fallback={<main class="pos-payment"><p class="pos-loading">Loading order…</p></main>}>
      <main class="pos-payment">
        <div class="pos-order-heading">
          <div><h1>{label(active()!)}</h1><p>{active()!.merchant_order_id ? 'Reference · ' : ''}Order {shortId(active()!.order_id)}</p></div>
          <StatusBadge order={active()!} offline={offline() && !terminal(active()!)}/>
        </div>
        <Show when={!terminal(active()!)} fallback={<Outcome order={active()!}/>}>
          <PaymentCard order={active()!}/>
        </Show>
        <Show when={error()}><p class="pos-error" role="alert">{error()}</p></Show>
        <Show when={!terminal(active()!)} fallback={<button type="button" class="pos-primary" onClick={resetKeypad}>New order</button>}>
          <button type="button" class="pos-primary" disabled={busy()} onClick={() => void backgroundOrder()}>Background order</button>
          <Show when={active()!.status === 'pending' && !active()!.error} fallback={<p class="pos-action-hint">Background keeps this payment open while you serve the next customer</p>}>
            <button type="button" class="pos-cancel" disabled={busy()} onClick={() => void cancelOrder()}>Cancel order</button>
            <p class="pos-action-hint">Background keeps this payment open · Cancel asks for confirmation</p>
          </Show>
        </Show>
      </main>
    </Show>}</Show>

    <Show when={screen() === 'list'}>
      <main class="pos-list" ref={el => { listElement = el; }}>
        <h1>Background orders</h1>
        <p class="pos-list-subtitle">Choose an order to open.</p>
        <div class="pos-search">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6"/><path d="m15 15 5 5"/></svg>
          <input class="pos-input" type="search" aria-label="Search reference or order ID" placeholder="Search reference or order ID" value={search()} onInput={event => onSearch(event.currentTarget.value)}/>
        </div>
        <div class="pos-tabs" role="tablist" aria-label="Order status">
          <button type="button" role="tab" aria-selected={tab() === 'active' ? 'true' : 'false'} class={tab() === 'active' ? 'selected' : ''} onClick={() => { setTab('active'); queueMicrotask(openStream); }}>Active · {activeCount()}{nextOffset() < total() ? '+' : ''}</button>
          <button type="button" role="tab" aria-selected={tab() === 'finished' ? 'true' : 'false'} class={tab() === 'finished' ? 'selected' : ''} onClick={() => { setTab('finished'); queueMicrotask(openStream); }}>Finished · {finishedCount()}{nextOffset() < total() ? '+' : ''}</button>
        </div>
        <Show when={loading()}><p class="pos-empty">Loading orders…</p></Show>
        <Show when={searching()}><p class="pos-empty">Searching orders…</p></Show>
        <Show when={error()}><p class="pos-error" role="alert">{error()} <button type="button" class="pos-link" onClick={() => void refresh()}>Retry</button></p></Show>
        <Show when={!loading() && !searching() && !error() && visible().length === 0}><p class="pos-empty">{search() ? 'No matching orders.' : `No ${tab()} orders yet.`}</p></Show>
        <div class="pos-list-items"><For each={visible()}>{order => <article class="pos-order-card">
          <div class="pos-order-card-head">
            <div><h2>{label(order)}</h2><p>{orderLine(order)}</p></div>
            <StatusBadge order={order} offline={offline() && !terminal(order)}/>
          </div>
          <p class="pos-order-sum">{order.amount} <span>{order.currency}</span></p>
          <div class="pos-order-foot"><small>{cardDetail(order)}</small><button type="button" class="pos-link" onClick={() => void openOrder(order)}>Open →</button></div>
        </article>}</For></div>
        <Show when={search().trim() ? searchOffset() < searchTotal() : nextOffset() < total()}>
          <button type="button" class="pos-load-more" onClick={() => void (search().trim() ? refreshSearch(true) : refresh(true))}>Load more orders</button>
        </Show>
      </main>
    </Show>
  </>;
}

render(() => <App/>, root);

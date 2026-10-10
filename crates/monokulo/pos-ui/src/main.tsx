import { createEffect, createMemo, createSignal, flush, For, onSettled, Show, untrack } from 'solid-js';
import { render } from '@solidjs/web';
import { StatusBadge, StatusIcon, StatusSymbols, stateOf, statusName } from './status';
import { addressFromQr, decodeImageFile, looksLikeAddress, scanCamera } from './refund';
import { LOADING_MARK } from './logo';
import { applyTheme, type Theme } from './theme';
import { createTimeline, routeOf, watchPage } from './timeline';
import './pos.css';

type Order = {
  /** `order_id_short` and `address_short` are the server's shortened forms
   * (`views::short_id_text`, `views::short_address_text`): the app never cuts
   * an identifier itself. */
  order_id: string; order_id_short: string; merchant_order_id: string | null; address: string; address_short: string;
  xmr_amount: string; amount: string; currency: string; status: string;
  confirmations: number; confirmations_required: number; error: string | null;
  updated_at?: number;
  backgrounded: boolean; cancelled_at: number | null; created_at: number; expires_at: number;
  received_xmr?: string; remaining_xmr?: string;
  refund_address?: string | null; qr_svg?: string;
};
type StatusEvent = Pick<Order, 'order_id' | 'status' | 'confirmations' | 'confirmations_required' | 'error' | 'updated_at' | 'received_xmr' | 'remaining_xmr' | 'qr_svg'> & { is_terminal: boolean };
type Config = { connectionId: string; publicKey: string; currency: string; decimals: number; storeName: string; clientLogging: boolean };

const root = document.getElementById('pos-root');
if (!root) throw new Error('POS root missing');
const config: Config = {
  connectionId: root.dataset.connectionId || '', publicKey: root.dataset.publicKey || '',
  currency: root.dataset.currency || 'AUD', decimals: Number(root.dataset.decimals || '2'),
  storeName: root.dataset.storeName || 'Store', clientLogging: root.dataset.clientLogging === 'true',
};
const api = `/dashboard/stores/${encodeURIComponent(config.connectionId)}/pos`;
/** This session's story for the store's logs, when it opted in. */
const timeline = createTimeline(config.clientLogging, `${api}/logs`);
watchPage(timeline);
const terminal = (o: Order) => Boolean(o.cancelled_at) || ['paid', 'overpaid', 'expired'].includes(o.status);

/** `#a8723b…b0d44e` - the order ID as the server shortened it, marked with `#`. */
const shortId = (o: Order) => `#${o.order_id_short}`;
const label = (o: Order) => o.merchant_order_id || shortId(o);
/** An order's name in full: its reference, or else its whole ID (for a title or a screen reader). */
const fullLabel = (o: Order) => o.merchant_order_id || o.order_id;
/** A value as the server shortened it, in the site's own markup
 * (`views::short_id`, site.css `.short-value`): the short text on screen,
 * the whole over it to double-click and copy, and for a screen reader. */
function Shortened(props: { short: string; full: string }) {
  return <Show when={props.short !== props.full} fallback={props.full}>
    <span class="short-value" title={props.full}><span class="short-value-text" aria-hidden="true">{props.short}</span><span class="short-value-full">{props.full}</span></span>
  </Show>;
}
/** An order's ID shortened, `#` first. */
const OrderId = (props: { order: Order }) => <Shortened short={shortId(props.order)} full={props.order.order_id}/>;
/** An order's name: its reference, or else its ID shortened. */
const Label = (props: { order: Order }) => <Show when={props.order.merchant_order_id} fallback={<OrderId order={props.order}/>}>{props.order.merchant_order_id}</Show>;
/** An exact XMR amount without trailing zeros (`0.052431000000` → `0.052431`). */
const trimXmr = (value: string) => value.includes('.') ? value.replace(/0+$/, '').replace(/\.$/, '') : value;
/** An order's amount as shown: an XMR amount without trailing zeros. */
const shownAmount = (o: { amount: string; currency: string }) => o.currency === 'XMR' ? trimXmr(o.amount) : o.amount;
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

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const started = performance.now();
  const method = init?.method || 'GET';
  let response: Response;
  try { response = await fetch(url, init); }
  catch (e) {
    timeline.record('request.failed', { method, route: routeOf(url), ms: Math.round(performance.now() - started), error: (e as Error).message }, 'warn');
    throw e;
  }
  timeline.record('request', { method, route: routeOf(url), status: response.status, ms: Math.round(performance.now() - started) }, response.ok ? 'info' : 'warn');
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

/** The clock countdowns and the Finished tab read; App ticks it. */
const [now, setNow] = createSignal(Math.floor(Date.now() / 1000));
/** How long an order stays in the Finished tab after it finished here. */
const FINISHED_KEEP_SECONDS = 24 * 60 * 60;
/** How long the POS waits to reopen an update stream the browser gave up
 * on: 5s, doubling with each failure to at most a minute, as the checkout
 * (static/checkout.js) does. */
const STREAM_RETRY_MS = 5000;
const STREAM_RETRY_MAX_MS = 60000;

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

/** Moves the site's logo and name (rendered by the server into
 * #pos-site-brand, so the POS draws the same mark as the site nav) to the
 * start of the top bar. */
function adoptSiteBrand(slot: HTMLElement) {
  const brand = document.getElementById('pos-site-brand');
  if (!brand) return;
  slot.append(...Array.from(brand.children));
  brand.remove();
}

type Step = 'done' | 'now' | 'part' | 'fail' | 'todo';
const stepWords: Record<Step, string> = { done: 'done', now: 'now', part: 'partly done', fail: 'stopped', todo: 'to come' };
/** Whether the customer still owes something: only then is the code live. */
const awaitingPayment = (o: Order) => ['pending', 'partial'].includes(o.status) && !o.cancelled_at;

/** The stage's track: Send, Confirm, Paid - without Confirm when the store
 * counts a payment as soon as it's seen (`confirmations_required` 0). */
function stageSteps(o: Order): [string, Step][] {
  const state = stateOf(o);
  const [send, confirm, paid]: Step[] =
    o.cancelled_at || (state === 'double-spend' && !awaitingPayment(o)) ? ['fail', 'todo', 'todo']
    : o.status === 'pending' ? ['now', 'todo', 'todo']
    : o.status === 'partial' ? ['part', 'todo', 'todo']
    : ['unconfirmed', 'confirming'].includes(o.status) ? ['done', 'now', 'todo']
    : ['paid', 'overpaid'].includes(o.status) ? ['done', 'done', 'done']
    : ['fail', 'todo', 'todo'];
  return o.confirmations_required === 0
    ? [['Send', send], ['Paid', confirm === 'now' ? 'now' : paid]]
    : [['Send', send], ['Confirm', confirm], ['Paid', paid]];
}

/** The stage's words: a short title, then what to do now. */
function stageMessage(o: Order, at: number): [string, string] {
  const received = trimXmr(o.received_xmr || '0'), total = trimXmr(o.xmr_amount), rest = trimXmr(o.remaining_xmr || o.xmr_amount);
  const left = `${durationUntil(o.expires_at, at)} left`;
  if (o.cancelled_at) return ['Cancelled', o.status === 'pending' ? 'If money still arrives at its address, it will show in the order for review.' : 'Payment activity arrived after this order was cancelled. Review it in the order details.'];
  switch (stateOf(o)) {
    case 'double-spend': return ['Payment reversed', o.error || 'Double spend detected. Do not treat this payment as paid.'];
    case 'pending': return [`Send ${total} XMR`, `Scan the code or copy the address. ${left}`];
    case 'partial': return [`Send the remaining ${rest} XMR`, `${received} of ${total} XMR received. This new code asks for the rest. ${left}`];
    case 'unconfirmed': return ['Waiting for confirmation', 'Payment seen. Waiting for its first confirmation.'];
    case 'confirming': return ['Confirming', `${o.confirmations} of ${o.confirmations_required} confirmations.`];
    case 'paid': return ['Paid', `${trimXmr(o.received_xmr || o.xmr_amount)} XMR received${o.confirmations_required ? ' and confirmed' : ''}.`];
    case 'overpaid': return ['Paid, with extra', o.error || 'More than the order amount arrived. Review the extra in the order.'];
    case 'expired': return ['Expired', o.error || 'This order expired before it was paid.'];
    default: return [statusName[stateOf(o)] || o.status, o.error || ''];
  }
}

const trackIcon = (step: Step) => step === 'done'
  ? <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="m4.5 12.5 5 5 10-11"/></svg>
  : step === 'fail' ? <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round"><path d="M7 7l10 10M17 7 7 17"/></svg> : null;

/** The stage at the top of the payment card, in every state (the checkout
 * shows the same): where the order is, and what to do now. */
function Stage(props: { order: Order }) {
  const steps = () => stageSteps(props.order);
  const message = () => stageMessage(props.order, now());
  return <div class={['pos-stage', `state-${stateOf(props.order)}`]}>
    <ol class={['pos-track', { 'pos-track-2': steps().length === 2 }]} aria-label="Payment progress">
      {steps().map(([name, step]) => <li class={`step-${step}`}>
        <span class="pos-track-dot" aria-hidden="true">{trackIcon(step)}</span>
        <span class="pos-track-name">{name}<span class="sr-only">, {stepWords[step]}</span></span>
      </li>)}
    </ol>
    <p class="pos-stage-msg" role="status"><strong>{message()[0]}.</strong> {message()[1]}</p>
  </div>;
}

/** Sketch 2's payment card: what the customer pays, where, and where a
 * refund would go - in every state, with the stage on top. Once the code
 * shouldn't be paid (the payment is in, or the order is over) it fades. */
function PaymentCard(props: { order: Order }) {
  // A memo, so an order refresh carrying the same QR doesn't rebuild it.
  const qr = createMemo(() => props.order.qr_svg);
  const [copied, setCopied] = createSignal(false);
  // Seeded once from the order: later edits are the merchant's, not the order's.
  const initialRefund = untrack(() => props.order.refund_address || '');
  const [refund, setRefund] = createSignal(initialRefund);
  const [saved, setSaved] = createSignal(initialRefund);
  const [refundState, setRefundState] = createSignal<'idle' | 'saving' | 'saved' | 'invalid'>(initialRefund ? 'saved' : 'idle');
  const [refundMessage, setRefundMessage] = createSignal('');
  const [scanning, setScanning] = createSignal(false);
  let video: HTMLVideoElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let stopScan: (() => void) | null = null;
  let saveTimer: number | undefined;
  onSettled(() => () => { stopScan?.(); window.clearTimeout(saveTimer); });

  // Awaiting: the customer still has to pay (all of it, or the rest).
  // Otherwise the code fades rather than inviting a second payment.
  const awaiting = () => awaitingPayment(props.order);
  const partial = () => props.order.status === 'partial' && !props.order.cancelled_at;

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
      timeline.record(response.ok ? 'refund.saved' : 'refund.rejected', { order_id: props.order.order_id, status: response.status }, response.ok ? 'info' : 'warn');
      if (refund().trim() !== value) return;
      if (response.ok) { setSaved(value); setRefundState('saved'); }
      else { setRefundState('invalid'); setRefundMessage(body.error || 'That address was not accepted.'); }
    } catch (e) {
      timeline.record('refund.save_failed', { order_id: props.order.order_id, error: (e as Error).message }, 'warn');
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
    catch (e) {
      timeline.record('refund.camera_failed', { order_id: props.order.order_id, error: (e as Error)?.message }, 'warn');
      setRefundMessage('Camera unavailable. Choose a QR image instead.');
    }
    finally { setScanning(false); stopScan = null; }
  }
  async function chooseImage(file: File | undefined) {
    if (!file) return;
    setRefundMessage('');
    try { useQr(await decodeImageFile(file)); }
    catch { setRefundMessage('Could not read that image. Choose another file.'); }
    if (fileInput) fileInput.value = '';
  }

  return <section class={['pos-pay-card', { 'is-spent': !awaiting() }]} aria-label="Payment details">
    <Stage order={props.order}/>
    <p class="pos-pay-caption">{partial() ? 'Still to pay' : awaiting() ? 'Send exactly this amount' : 'Order total'}</p>
    <p class="pos-pay-xmr">{trimXmr(partial() ? props.order.remaining_xmr || props.order.xmr_amount : props.order.xmr_amount)} <span>XMR</span></p>
    <Show when={props.order.currency !== 'XMR' && !partial()}><p class="pos-pay-fiat">≈ {props.order.amount} {props.order.currency}</p></Show>
    {/* After a partial payment the code is redrawn for the rest, and says so. */}
    <Show when={qr()}>
      <Show when={partial()} fallback={<div class="pos-qr" innerHTML={qr()}/>}>
        <div class="pos-qr-new"><span class="pos-qr-new-tab">New code · {trimXmr(props.order.remaining_xmr || props.order.xmr_amount)} XMR</span><div class="pos-qr" innerHTML={qr()}/></div>
      </Show>
    </Show>
    <p class="pos-quiet-label">Payment address</p>
    <div class="pos-address">
      <code><Shortened short={props.order.address_short} full={props.order.address}/></code>
      <Show when={awaiting()}><button type="button" onClick={() => void copy()} aria-label="Copy payment address">{copied() ? 'Copied' : 'Copy'}</button></Show>
    </div>
    <hr/>
    <label class="pos-field-label" for="pos-refund">Refund address <span>(optional)</span></label>
    <div class={['pos-refund', `state-${refundState()}`]}>
      <input id="pos-refund" type="text" autocomplete="off" spellcheck={false} placeholder="Your Monero refund address"
        value={refund()} onInput={event => onRefundInput(event.currentTarget.value)} onBlur={() => void save(refund().trim())}
        aria-invalid={refundState() === 'invalid' ? 'true' : 'false'} aria-describedby="pos-refund-note"/>
      <span class="pos-refund-state" role="status" aria-label={refundState() === 'saving' ? 'Saving refund address' : refundState() === 'saved' ? 'Refund address saved' : ''}>
        <Show when={refundState() === 'saved'}><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 5 5L19 7"/></svg></Show>
        <Show when={refundState() === 'saving'}><span class="pos-loading-mark" innerHTML={LOADING_MARK}/></Show>
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

function App() {
  const [orders, setOrders] = createSignal<Order[]>([]);
  const [screen, setScreen] = createSignal<'keypad' | 'payment' | 'list'>('keypad');
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [digits, setDigits] = createSignal('0');
  const [reference, setReference] = createSignal('');
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [offline, setOffline] = createSignal(false);
  const [search, setSearch] = createSignal('');
  const [tab, setTab] = createSignal<'active' | 'finished'>('active');
  const [listScroll, setListScroll] = createSignal(0);
  // When each order finished on this device (unix seconds). The Finished
  // tab is only these, for 24 hours; nothing is loaded into it, so a reload
  // starts it empty. The store's orders page is the full history.
  const [finishedAt, setFinishedAt] = createSignal<Record<string, number>>({});
  // Orders seen open this session: only these can move to Finished.
  const seenOpen = new Set<string>();
  const active = createMemo(() => orders().find(o => o.order_id === activeId()) || null);
  const background = createMemo(() => orders().filter(o => !terminal(o) && o.order_id !== activeId()));
  const activeOrders = createMemo(() => orders().filter(o => !terminal(o)));
  const finishedOrders = createMemo(() => {
    const at = finishedAt();
    return orders().filter(o => terminal(o) && at[o.order_id] !== undefined && now() - at[o.order_id] < FINISHED_KEEP_SECONDS)
      .sort((a, b) => at[b.order_id] - at[a.order_id]);
  });
  const visible = createMemo(() => {
    const term = search().trim().toLowerCase();
    return (tab() === 'active' ? activeOrders() : finishedOrders())
      .filter(o => !term || o.order_id.toLowerCase().includes(term) || (o.merchant_order_id || '').toLowerCase().includes(term));
  });
  const amountText = createMemo(() => displayAmount(digits()));
  // A live snapshot can arrive while an HTTP read is in flight. Unix-second
  // timestamps cannot order those responses, and reorgs can regress status.
  const statusRevisions = new Map<string, number>();
  const statusRevision = (id: string) => statusRevisions.get(id) ?? 0;
  let lostTimer: number | undefined;
  let listElement: HTMLElement | undefined;
  let pendingRequest: { amount: string; reference: string; key: string } | null = null;

  /** The store's full orders page, optionally searching for `term`. */
  const allOrdersUrl = (term = '') => `/dashboard/stores/${encodeURIComponent(config.connectionId)}/orders${term ? `?q=${encodeURIComponent(term)}` : ''}`;
  function merge(previous: Order, next: Partial<Order> & { updated_at?: number }): Order {
    if (previous.updated_at !== undefined && next.updated_at !== undefined && next.updated_at < previous.updated_at) return previous;
    return { ...previous, ...next, qr_svg: next.qr_svg ?? previous.qr_svg, refund_address: next.refund_address !== undefined ? next.refund_address : previous.refund_address };
  }
  function mergeRead(previous: Order, next: Order, revision: number): Order {
    const merged = merge(previous, next);
    if (statusRevision(next.order_id) === revision) return merged;
    // Keep status received after this read began, while filling in details
    // (such as the initial QR) that the live snapshot may not include.
    return { ...merged, status: previous.status, confirmations: previous.confirmations,
      confirmations_required: previous.confirmations_required, error: previous.error,
      updated_at: previous.updated_at, received_xmr: previous.received_xmr,
      remaining_xmr: previous.remaining_xmr, cancelled_at: previous.cancelled_at,
      qr_svg: previous.qr_svg ?? merged.qr_svg };
  }
  /** Every change to the orders goes through here, so an order that turns
   * final is noted as finished at that moment. */
  function updateOrders(change: (previous: Order[]) => Order[]) {
    // Writes are batched until the next microtask; flush() applies this one
    // now so the list read back below is the updated one.
    const before = new Map(orders().map(o => [o.order_id, o]));
    setOrders(change);
    flush();
    const next = orders();
    for (const o of next) {
      const was = before.get(o.order_id);
      if (was && (was.status !== o.status || Boolean(was.cancelled_at) !== Boolean(o.cancelled_at) || was.error !== o.error)) {
        timeline.record('order.status', {
          order_id: o.order_id, from: was.status, to: o.status, confirmations: o.confirmations,
          cancelled: Boolean(o.cancelled_at), error: o.error ?? undefined,
        }, stateOf(o) === 'double-spend' || o.error ? 'warn' : 'info');
      }
    }
    for (const o of next) if (!terminal(o)) seenOpen.add(o.order_id);
    const recorded = finishedAt();
    const newlyFinished = next.filter(o => terminal(o) && seenOpen.has(o.order_id) && recorded[o.order_id] === undefined);
    if (newlyFinished.length) {
      for (const o of newlyFinished) timeline.record('order.finished', { order_id: o.order_id, status: o.cancelled_at ? 'cancelled' : o.status });
      const at = Math.floor(Date.now() / 1000);
      setFinishedAt({ ...recorded, ...Object.fromEntries(newlyFinished.map(o => [o.order_id, at])) });
    }
  }
  function upsert(order: Order) {
    updateOrders(previous => previous.some(o => o.order_id === order.order_id)
      ? previous.map(o => o.order_id === order.order_id ? merge(o, order) : o) : [order, ...previous]);
  }
  /** Loads every open order (however old) and keeps this session's
   * finished ones. An order that was open here but is no longer is read
   * once more, so it moves to Finished with its final state. */
  async function refresh() {
    const revisions = new Map(statusRevisions);
    try {
      const data = await json<{ orders: Order[] }>(`${api}/orders?state=active`);
      const open = new Set(data.orders.map(o => o.order_id));
      const gone = orders().filter(o => !terminal(o) && !open.has(o.order_id)).map(o => o.order_id);
      updateOrders(previous => [
        ...data.orders.map(o => { const known = previous.find(p => p.order_id === o.order_id); return known ? mergeRead(known, o, revisions.get(o.order_id) ?? 0) : o; }),
        ...previous.filter(o => !open.has(o.order_id) && (terminal(o) || gone.includes(o.order_id))),
      ]);
      for (const id of gone) void loadOrder(id).catch(() => {});
      let resumed: string | undefined;
      if (!activeId()) {
        const foreground = data.orders.find(o => !o.backgrounded);
        if (foreground) { resumed = foreground.order_id; setActiveId(foreground.order_id); setScreen('payment'); void loadOrder(foreground.order_id).catch(() => {}); }
      }
      timeline.record('orders.loaded', { open: data.orders.length, backgrounded: data.orders.filter(o => o.backgrounded).length, order_id: resumed });
      setError('');
    } catch (e) { setError((e as Error).message); }
  }
  async function loadOrder(id: string) {
    const revision = statusRevision(id);
    const order = await json<Order>(`${api}/orders/${encodeURIComponent(id)}`);
    const known = orders().find(o => o.order_id === id);
    upsert(known ? mergeRead(known, order, revision) : order);
    return order;
  }
  /** The orders the live stream follows: the one on screen, those shown in
   * the stack or list, every other open one, and recently cancelled unpaid
   * ones (a payment may still arrive). As a comma-joined key, so the stream
   * reopens only when the set actually changes. */
  const watchedIds = createMemo(() => {
    const current = active();
    const foreground = current && !terminal(current) ? [current.order_id] : [];
    const onScreen = screen() === 'list' ? visible().filter(o => !terminal(o)).map(o => o.order_id) : background().map(o => o.order_id);
    const otherActive = activeOrders().map(o => o.order_id);
    const cancelled = orders().filter(o => o.cancelled_at && o.status === 'pending').slice(0, 8).map(o => o.order_id);
    return [...new Set([...foreground, ...onScreen, ...otherActive, ...cancelled])].slice(0, 32).join(',');
  });
  function markConnected() { window.clearTimeout(lostTimer); lostTimer = undefined; setOffline(false); }
  /** When the stream last dropped, for how long it was down. */
  let streamDownAt = 0;
  // One update stream for the watched orders, reopened whenever they change
  // and closed when the POS goes away. It sends every watched order's status
  // when it connects, so opening it needs no per-order reads (the engine
  // rate-limits each store).
  createEffect(watchedIds, ids => {
    if (!ids) { markConnected(); return; }
    const url = `${api}/events?orders=${ids.split(',').map(encodeURIComponent).join(',')}`;
    const watching = ids.split(',').length;
    let source: EventSource;
    let retryTimer: number | undefined;
    let retryDelay = STREAM_RETRY_MS;
    const open = () => {
      source = new EventSource(url);
      source.addEventListener('open', () => {
        timeline.record('stream.open', { orders: watching, down_ms: streamDownAt ? Date.now() - streamDownAt : undefined });
        streamDownAt = 0;
        retryDelay = STREAM_RETRY_MS;
        markConnected();
      });
      source.addEventListener('status', event => {
        let update: StatusEvent;
        try { update = JSON.parse((event as MessageEvent).data) as StatusEvent; }
        catch { return; /* A malformed event is ignored; the next snapshot reconciles. */ }
        statusRevisions.set(update.order_id, statusRevision(update.order_id) + 1);
        updateOrders(previous => previous.map(o => o.order_id === update.order_id ? merge(o, update) : o));
      });
      // A dropped stream is retried every few seconds; the counter is
      // offline once it has failed to reconnect for 6s, however many
      // attempts that took (and however often the watched set changes
      // meanwhile). Only a successful open clears it.
      source.addEventListener('error', () => {
        if (!streamDownAt) { streamDownAt = Date.now(); timeline.record('stream.error', { orders: watching, online: navigator.onLine }, 'warn'); }
        if (lostTimer === undefined) lostTimer = window.setTimeout(() => { setOffline(true); timeline.record('stream.lost', { after_ms: 6000 }, 'warn'); }, 6000);
        // A stream closed for good isn't retried by the browser: any browser's
        // after a refusal (a server error, a restart), and Firefox's after a
        // failed connection too, the Wi-Fi being down. The POS opens a new
        // one itself, waiting longer each time one fails.
        if (source.readyState === EventSource.CLOSED) {
          retryTimer = window.setTimeout(open, retryDelay);
          retryDelay = Math.min(retryDelay * 2, STREAM_RETRY_MAX_MS);
        }
      });
    };
    open();
    return () => { window.clearTimeout(retryTimer); source.close(); };
  });
  function resetKeypad() { setDigits('0'); setReference(''); setActiveId(null); setScreen('keypad'); setError(''); }
  function pushDigit(d: string) { setDigits(value => (value + d).slice(-(config.decimals + 9)).replace(/^0+(?=\d)/, '') || '0'); }
  function backspace() { setDigits(value => value.length > 1 ? value.slice(0, -1) : '0'); }
  async function charge() {
    if (busy() || /^0+$/.test(digits())) return;
    setBusy(true); setError('');
    const amount = plainAmount(digits());
    const note = reference().trim();
    const retry = Boolean(pendingRequest && pendingRequest.amount === amount && pendingRequest.reference === note);
    if (!retry || !pendingRequest) pendingRequest = { amount, reference: note, key: crypto.randomUUID() };
    // Whether a note was given and how long, never its text.
    timeline.record('order.charge', { amount, currency: config.currency, has_note: Boolean(note), note_length: note.length || undefined, retry });
    try {
      const created = await json<{ order_id: string }>(`${api}/orders`, {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ amount, merchant_order_id: note || null, request_key: pendingRequest.key }),
      });
      timeline.record('order.created', { order_id: created.order_id, has_note: Boolean(note) });
      await loadOrder(created.order_id);
      pendingRequest = null;
      setActiveId(created.order_id); setScreen('payment');
    } catch (e) { timeline.record('order.create_failed', { error: (e as Error).message }, 'warn'); setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function backgroundOrder() {
    const order = active(); if (!order || busy()) return;
    setBusy(true); setError('');
    try {
      await post(`${api}/orders/${encodeURIComponent(order.order_id)}/background`);
      timeline.record('order.backgrounded', { order_id: order.order_id, status: order.status });
      upsert({ ...order, backgrounded: true }); resetKeypad();
    } catch (e) { timeline.record('order.background_failed', { order_id: order.order_id, error: (e as Error).message }, 'warn'); setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function cancelOrder() {
    const order = active();
    if (!order || busy() || !window.confirm(`Cancel ${label(order)}? The payment address has already been issued; any later payment will still need review.`)) return;
    setBusy(true); setError('');
    try {
      await post(`${api}/orders/${encodeURIComponent(order.order_id)}/cancel`);
      timeline.record('order.cancelled', { order_id: order.order_id });
      await loadOrder(order.order_id);
    } catch (e) {
      timeline.record('order.cancel_failed', { order_id: order.order_id, error: (e as Error).message }, 'warn');
      setError((e as Error).message);
      // Refused because a payment arrived meanwhile: show it.
      void loadOrder(order.order_id).catch(() => {});
    }
    finally { setBusy(false); }
  }
  async function openOrder(order: Order) {
    timeline.record('order.foregrounded', { order_id: order.order_id, from: screen() === 'list' ? 'list' : 'stack', status: order.status });
    if (screen() === 'list' && listElement) setListScroll(listElement.scrollTop);
    setError(''); setActiveId(order.order_id); setScreen('payment');
    try { await loadOrder(order.order_id); } catch (e) { setError((e as Error).message); }
  }
  function showList() {
    setScreen('list');
    // Render the list now, so its old scroll position can be restored.
    flush();
    if (listElement) listElement.scrollTop = listScroll();
  }
  function onKey(event: KeyboardEvent) {
    if (screen() !== 'keypad') return;
    if (event.target instanceof HTMLInputElement) { if (event.key === 'Enter') void charge(); return; }
    if (/^[0-9]$/.test(event.key)) pushDigit(event.key);
    else if (event.key === 'Backspace') backspace();
    else if (event.key === 'Escape') setDigits('0');
    else if (event.key === 'Enter') void charge();
  }
  // Which screen the terminal shows, as it changes.
  createEffect(screen, shown => timeline.record('screen', { screen: shown, order_id: shown === 'payment' ? untrack(activeId) ?? undefined : undefined }));
  onSettled(() => {
    void refresh();
    document.addEventListener('keydown', onKey);
    const clockTick = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 15000);
    return () => { document.removeEventListener('keydown', onKey); window.clearInterval(clockTick); window.clearTimeout(lostTimer); };
  });

  const orderLine = (o: Order) => <>{o.merchant_order_id ? 'Reference · ' : ''}<OrderId order={o}/> · created {clock(o.created_at)}</>;
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
      <span class="pos-brand-slot" ref={adoptSiteBrand}/>
      <span class="pos-crumb-sep" aria-hidden="true">/</span>
      <Show when={screen() === 'list'} fallback={
        <a class="pos-store" href={`/dashboard/stores/${encodeURIComponent(config.connectionId)}`}>{config.storeName}</a>
      }>
        <button class="pos-back" type="button" onClick={() => setScreen('keypad')} aria-label="Back to POS">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 5-7 7 7 7"/></svg>
        </button>
      </Show>
      <span class="pos-mode">POS</span>
      <span class="pos-top-end">
        <Show when={screen() !== 'list'}>
          <button class="pos-orders-link" type="button" onClick={showList} aria-label="All orders" title="All orders">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1" fill="currentColor"/><circle cx="4.5" cy="12" r="1" fill="currentColor"/><circle cx="4.5" cy="18" r="1" fill="currentColor"/></svg>
          </button>
        </Show>
        <span class="pos-site-controls" ref={adoptSiteControls}/>
      </span>
    </header>

    {/* Above the keypad on a phone; a sidebar beside the keypad and the
        payment view on a tablet or desktop (hidden beside a phone's
        payment view, which has no room for it). */}
    <Show when={screen() !== 'list' && background().length > 0}>
      <section class={['pos-stack', { 'beside-payment': screen() === 'payment' }]} aria-label="Background orders">
        <div class="pos-stack-heading"><strong>Background orders · {background().length}</strong><button type="button" onClick={showList}>View all →</button></div>
        <div class="pos-stack-scroll" tabindex="0" aria-label="Background orders, scroll sideways" onWheel={event => { const el = event.currentTarget; if (el.scrollWidth > el.clientWidth && Math.abs(event.deltaY) > Math.abs(event.deltaX)) { el.scrollLeft += event.deltaY; event.preventDefault(); } }}>
          <For each={background()} keyed={o => o.order_id}>{order => <button type="button" class={['pos-stack-card', `state-${stateOf(order(), offline())}`]}
            title={`${fullLabel(order())} · ${statusName[stateOf(order(), offline())]} · ${shownAmount(order())} ${order().currency}`}
            aria-label={`Open ${fullLabel(order())}, ${statusName[stateOf(order(), offline())]}, ${shownAmount(order())} ${order().currency}`} onClick={() => void openOrder(order())}>
            <StatusIcon order={order()} offline={offline()}/><span class={order().merchant_order_id ? 'pos-stack-ref' : 'pos-stack-ref is-id'}>{label(order())}</span><span class="pos-stack-amount">{shownAmount(order())}</span>
          </button>}</For>
        </div>
      </section>
    </Show>

    <Show when={screen() === 'keypad'}>
      <main class={['pos-keypad', { 'has-stack': background().length > 0 }]}>
        <p class={['pos-amount', `len-${Math.min(4, Math.floor(amountText().length / 6))}`]} aria-live="polite">
          {amountText()}<span>{config.currency}</span>
        </p>
        <div class="pos-keys">
          <For each={['1', '2', '3', '4', '5', '6', '7', '8', '9', 'C', '0', '⌫']}>{key => <button type="button"
            class={{ clear: key === 'C', delete: key === '⌫' }} aria-label={key === 'C' ? 'Clear' : key === '⌫' ? 'Backspace' : key}
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

    {/* Keyed by order id, so each order gets a fresh payment card (a keyed
        function child must take the value, hence the unused _id). */}
    <Show when={screen() === 'payment' ? activeId() : null} keyed>{_id => <Show when={active()} fallback={<main class="pos-payment"><p class="pos-loading">Loading order…</p></main>}>{order => (
      <main class="pos-payment">
        <div class="pos-order-heading">
          <div><h1><Label order={order()}/></h1><p>{order().merchant_order_id ? 'Reference · ' : ''}Order <OrderId order={order()}/></p></div>
          {/* The status at a glance; the stage in the card says the rest,
              the time left included. */}
          <div class="pos-status-row">
            <StatusBadge order={order()} offline={offline() && !terminal(order())}/>
          </div>
        </div>
        <PaymentCard order={order()}/>
        <div class="pos-actions">
          <Show when={error()}><p class="pos-error" role="alert">{error()}</p></Show>
          <Show when={!terminal(order())} fallback={<button type="button" class="pos-primary" onClick={resetKeypad}>New order</button>}>
            <button type="button" class="pos-primary" disabled={busy()} onClick={() => void backgroundOrder()}>Background order</button>
            <Show when={order().status === 'pending' && !order().error} fallback={<p class="pos-action-hint">Background keeps this payment open while you serve the next customer</p>}>
              <button type="button" class="pos-cancel" disabled={busy()} onClick={() => void cancelOrder()}>Cancel order</button>
              <p class="pos-action-hint">Background keeps this payment open · Cancel asks for confirmation</p>
            </Show>
          </Show>
        </div>
      </main>
    )}</Show>}</Show>

    <Show when={screen() === 'list'}>
      <main class="pos-list" ref={el => { listElement = el; }}>
        <h1>Orders</h1>
        <p class="pos-list-subtitle">Choose an order to open.</p>
        <div class="pos-search">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6"/><path d="m15 15 5 5"/></svg>
          <input class="pos-input" type="search" aria-label="Search reference or order ID" placeholder="Search reference or order ID" value={search()} onInput={event => setSearch(event.currentTarget.value)}/>
        </div>
        <div class="pos-tabs" role="tablist" aria-label="Order status">
          <button type="button" role="tab" aria-selected={tab() === 'active' ? 'true' : 'false'} class={{ selected: tab() === 'active' }} onClick={() => setTab('active')}>Active · {activeOrders().length}</button>
          <button type="button" role="tab" aria-selected={tab() === 'finished' ? 'true' : 'false'} class={{ selected: tab() === 'finished' }} onClick={() => setTab('finished')}>Finished · {finishedOrders().length}</button>
        </div>
        <Show when={tab() === 'finished'}>
          <p class="pos-list-note">Completed on this device since the POS was opened. They clear after 24 hours or when the page reloads. <a href={allOrdersUrl()}>See all orders →</a></p>
        </Show>
        <Show when={error()}><p class="pos-error" role="alert">{error()} <button type="button" class="pos-link" onClick={() => void refresh()}>Retry</button></p></Show>
        <Show when={!error() && visible().length === 0}>
          <Show when={search().trim()} fallback={<p class="pos-empty">{`No ${tab()} orders yet.`}</p>}>
            <p class="pos-empty">No matches from this session. <a href={allOrdersUrl(search().trim())}>Search all orders →</a></p>
          </Show>
        </Show>
        <div class="pos-list-items"><For each={visible()} keyed={o => o.order_id}>{order => <article class="pos-order-card">
          <div class="pos-order-card-head">
            <div><h2><Label order={order()}/></h2><p>{orderLine(order())}</p></div>
            <StatusBadge order={order()} offline={offline() && !terminal(order())}/>
          </div>
          <p class="pos-order-sum">{shownAmount(order())} <span>{order().currency}</span></p>
          <div class="pos-order-foot"><small>{cardDetail(order())}</small><button type="button" class="pos-link" onClick={() => void openOrder(order())}>Open →</button></div>
        </article>}</For></div>
      </main>
    </Show>
  </>;
}

render(() => <App/>, root);

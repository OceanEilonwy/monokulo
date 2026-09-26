import { createMemo, createSignal, For, onCleanup, Show } from 'solid-js';
import { render } from '@solidjs/web';
import './pos.css';

type Order = {
  order_id: string; merchant_order_id: string | null; address: string;
  xmr_amount: string; amount: string; currency: string; status: string;
  confirmations: number; confirmations_required: number; error: string | null;
  updated_at?: number;
  backgrounded: boolean; cancelled_at: number | null; created_at: number; expires_at: number;
};
type StatusEvent = Pick<Order, 'order_id' | 'status' | 'confirmations' | 'confirmations_required' | 'error' | 'updated_at'> & { is_terminal: boolean };
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
const stateOf = (o: Order, offline = false) => offline ? 'offline' : o.cancelled_at ? 'cancelled' :
  o.error?.includes('Double-spend') ? 'double-spend' : o.status === 'confirming' && o.confirmations === 0 ? 'unconfirmed' : o.status;
const statusName: Record<string, string> = {
  pending: 'Awaiting payment', unconfirmed: 'Unconfirmed', confirming: 'Confirming', partial: 'Partially paid',
  paid: 'Paid', overpaid: 'Overpaid', expired: 'Expired', cancelled: 'Cancelled',
  'double-spend': 'Double spend', offline: 'Connection lost',
};
const shortId = (id: string) => id.length <= 14 ? id : `${id.slice(0, 6)}…${id.slice(-4)}`;
const label = (o: Order) => o.merchant_order_id || shortId(o.order_id);
const plainAmount = (digits: string) => {
  const padded = digits.padStart(config.decimals + 1, '0');
  return `${padded.slice(0, -config.decimals) || '0'}.${padded.slice(-config.decimals)}`;
};
const displayAmount = (digits: string) => plainAmount(digits).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
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

function Symbols() {
  return <svg class="pos-symbol-defs" aria-hidden="true" xmlns="http://www.w3.org/2000/svg">
    <defs>
      <clipPath id="pos-coin-large-fragment"><path d="M0 0h21.5l-3.5 9.5 3.5 5.9L17.8 32H0Z"/></clipPath>
      <symbol id="pos-coin" viewBox="0 0 32 32">
        <path d="M4 12.5v6c0 5 5.4 9 12 9s12-4 12-9v-6" fill="currentColor" fill-opacity=".24" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>
        <path d="M8 22.5v3M16 24.5v3M24 22.5v3" fill="none" stroke="currentColor" stroke-opacity=".55" stroke-width="1"/>
        <ellipse cx="16" cy="12.5" rx="12" ry="9" fill="#fff" stroke="currentColor" stroke-width="1.6"/>
        <path d="M9.5 15.7V9.4l6.5 5 6.5-5v6.3" fill="none" stroke="#ff6600" stroke-width="2.5" stroke-linecap="square"/>
        <path d="M9.5 15.7v1.2h13v-1.2" fill="none" stroke="currentColor" stroke-width="1"/>
      </symbol>
      <symbol id="pos-coin-partial" viewBox="0 0 32 32">
        <use href="#pos-coin" clip-path="url(#pos-coin-large-fragment)"/>
        <path d="M21.5 4 18 9.5 21.5 15.4 17.8 27" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
        <path d="M21.5 4.2C25.6 6 28 8.9 28 12.5v6c0 4.7-4.2 8.1-10.2 8.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-dasharray="2.2 2.2" stroke-linecap="round"/>
      </symbol>
      <symbol id="pos-coins-overpaid" viewBox="0 0 45 32"><use href="#pos-coin" x="0" y="3" width="31" height="29"/><use href="#pos-coin" x="13" y="0" width="31" height="29"/></symbol>
    </defs>
  </svg>;
}

function StatusIcon(props: { order: Order; offline?: boolean }) {
  const state = () => stateOf(props.order, props.offline);
  const progress = () => props.order.confirmations_required <= 0 ? 100 :
    Math.min(100, Math.max(20, Math.ceil(10 * props.order.confirmations / props.order.confirmations_required) * 10));
  return <span class={`pos-icon pos-icon-${state()}`} aria-hidden="true">
    <Show when={state() === 'pending'}><span class="pos-spinner"/></Show>
    <Show when={state() === 'unconfirmed'}><span class="pos-disc pos-disc-empty"/></Show>
    <Show when={state() === 'confirming'}><span class="pos-disc" style={{ '--progress': `${progress()}%` }}/></Show>
    <Show when={state() === 'partial' || state() === 'paid' || state() === 'overpaid'}>
      <svg viewBox={state() === 'overpaid' ? '0 0 45 32' : '0 0 32 32'}><use href={state() === 'partial' ? '#pos-coin-partial' : state() === 'overpaid' ? '#pos-coins-overpaid' : '#pos-coin'}/></svg>
    </Show>
    <Show when={state() === 'expired'}><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M5 2h14M5 22h14M7 2v4c0 3 5 5 5 6s-5 3-5 6v4m10-20v4c0 3-5 5-5 6s5 3 5 6v4M8 18h8l2 3H6z"/></svg></Show>
    <Show when={state() === 'double-spend'}><span class="pos-exclaim">!</span></Show>
    <Show when={state() === 'offline'}><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 9a15 15 0 0 1 18 0M6 12a10 10 0 0 1 5-1M18 12l1 1M9 16a5 5 0 0 1 6 0M12 20h.01M3 3l18 18"/></svg></Show>
    <Show when={state() === 'cancelled'}><span class="pos-cross">×</span></Show>
  </span>;
}

function Badge(props: { order: Order; offline?: boolean }) {
  return <span class={`pos-badge state-${stateOf(props.order, props.offline)}`}>
    <StatusIcon order={props.order} offline={props.offline}/>{statusName[stateOf(props.order, props.offline)] || props.order.status}
  </span>;
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
  const [tab, setTab] = createSignal<'active' | 'finished'>('active');
  const [total, setTotal] = createSignal(0);
  const [nextOffset, setNextOffset] = createSignal(0);
  const [listScroll, setListScroll] = createSignal(0);
  const active = createMemo(() => orders().find(o => o.order_id === activeId()) || null);
  const background = createMemo(() => orders().filter(o => o.backgrounded && !terminal(o)));
  const listed = createMemo(() => orders().filter(o => o.backgrounded));
  const activeCount = createMemo(() => listed().filter(o => !terminal(o)).length);
  const finishedCount = createMemo(() => listed().filter(terminal).length);
  const visible = createMemo(() => listed().filter(o => (tab() === 'active') !== terminal(o))
    .filter(o => `${o.merchant_order_id || ''} ${o.order_id}`.toLowerCase().includes(search().trim().toLowerCase())));
  let stream: EventSource | null = null;
  let lostTimer: number | undefined;
  let listElement: HTMLElement | undefined;
  let pendingRequest: { amount: string; reference: string; key: string } | null = null;

  function upsert(order: Order) {
    setOrders(previous => {
      const found = previous.some(o => o.order_id === order.order_id);
      return found ? previous.map(o => o.order_id === order.order_id ?
        (o.updated_at !== undefined && order.updated_at !== undefined && order.updated_at < o.updated_at ? o : order) : o) : [order, ...previous];
    });
  }
  async function refresh(append = false) {
    try {
      const offset = append ? nextOffset() : 0;
      const data = await json<{ orders: Order[]; total: number }>(`${api}/orders?offset=${offset}&limit=40`);
      setTotal(data.total);
      setNextOffset(offset + data.orders.length);
      setOrders(previous => append ? [...previous, ...data.orders.filter(o => !previous.some(p => p.order_id === o.order_id))] : data.orders);
      if (!append && !activeId()) {
        const foreground = data.orders.find(o => !o.backgrounded && !terminal(o));
        if (foreground) { setActiveId(foreground.order_id); setScreen('payment'); }
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
  function openStream() {
    stream?.close(); stream = null;
    const activeIds = orders().filter(o => !terminal(o)).map(o => o.order_id);
    const cancelledIds = orders().filter(o => o.cancelled_at && o.status === 'pending').slice(0, 8).map(o => o.order_id);
    const ids = [...activeIds, ...cancelledIds].slice(0, 32);
    if (!ids.length) { setOffline(false); return; }
    stream = new EventSource(`${api}/events?orders=${ids.map(encodeURIComponent).join(',')}`);
    stream.addEventListener('open', () => { window.clearTimeout(lostTimer); setOffline(false); void refreshStatuses(); });
    stream.addEventListener('status', event => {
      try {
        const update = JSON.parse((event as MessageEvent).data) as StatusEvent;
        setOrders(previous => previous.map(o => o.order_id === update.order_id ?
          (o.updated_at !== undefined && update.updated_at !== undefined && update.updated_at < o.updated_at ? o : { ...o, ...update }) : o));
        if (update.is_terminal) queueMicrotask(openStream);
      } catch { /* Ignore malformed event; next snapshot will reconcile. */ }
    });
    stream.addEventListener('error', () => {
      window.clearTimeout(lostTimer);
      lostTimer = window.setTimeout(() => setOffline(true), 6000);
    });
  }
  async function refreshStatuses() {
    const ids = orders().filter(o => !terminal(o)).slice(0, 32).map(o => o.order_id);
    const results = await Promise.allSettled(ids.map(loadOrder));
    if (results.some(r => r.status === 'rejected')) setOffline(true);
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
    const order = active(); if (!order || busy() || !window.confirm(`Cancel ${label(order)}? The payment address has already been issued; any later payment will still need review.`)) return;
    setBusy(true); setError('');
    try {
      await post(`${api}/orders/${encodeURIComponent(order.order_id)}/cancel`);
      await loadOrder(order.order_id); queueMicrotask(openStream);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function openOrder(order: Order) {
    if (screen() === 'list' && listElement) setListScroll(listElement.scrollTop);
    setError(''); setActiveId(order.order_id); setScreen('payment');
    try { await loadOrder(order.order_id); } catch (e) { setError((e as Error).message); }
  }
  function showList() { setScreen('list'); queueMicrotask(() => { if (listElement) listElement.scrollTop = listScroll(); }); }
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
  onCleanup(() => { document.removeEventListener('keydown', onKey); stream?.close(); window.clearTimeout(lostTimer); });

  return <><Symbols/>
    <header class="pos-top">
      <Show when={screen() === 'list'} fallback={<a class="pos-store" href={`/dashboard/stores/${encodeURIComponent(config.connectionId)}`}>{config.storeName}</a>}>
        <button class="pos-back" onClick={() => setScreen('keypad')} aria-label="Back to POS">←</button>
        <span class="pos-store">POS</span>
      </Show>
      <span class="pos-chevron">›</span><strong>{screen() === 'list' ? 'Orders' : 'POS'}</strong>
      <span class="pos-health" aria-label={offline() ? 'Connection lost' : 'Connected'}/>
    </header>
    <Show when={screen() === 'keypad' && listed().length > 0}>
      <section class="pos-stack" aria-label="Background orders">
        <div class="pos-stack-heading"><strong>Background orders · {background().length}</strong><button onClick={showList}>View all →</button></div>
        <div class="pos-stack-scroll" onWheel={event => { if (event.currentTarget.scrollWidth > event.currentTarget.clientWidth && Math.abs(event.deltaY) > Math.abs(event.deltaX)) { event.currentTarget.scrollLeft += event.deltaY; event.preventDefault(); } }}>
          <For each={background()}>{order => <button class={`pos-stack-card state-${stateOf(order, offline())}`} title={`${label(order)} · ${statusName[stateOf(order, offline())]} · ${order.amount} ${order.currency}`} aria-label={`Open ${label(order)}, ${statusName[stateOf(order, offline())]}, ${order.amount} ${order.currency}`} onClick={() => void openOrder(order)}>
            <StatusIcon order={order} offline={offline()}/><strong class="pos-stack-ref">{label(order)}</strong><span>{order.amount}</span>
          </button>}</For>
        </div>
      </section>
    </Show>
    <Show when={screen() === 'keypad'}>
      <main class={`pos-keypad ${listed().length ? 'has-stack' : ''}`}>
        <div class="pos-amount">{displayAmount(digits())}<span>{config.currency}</span></div>
        <div class="pos-keys">
          <For each={['1','2','3','4','5','6','7','8','9','C','0','⌫']}>{key => <button class={key === 'C' ? 'clear' : key === '⌫' ? 'delete' : ''} aria-label={key === 'C' ? 'Clear' : key === '⌫' ? 'Backspace' : key} onClick={() => key === 'C' ? setDigits('0') : key === '⌫' ? backspace() : pushDigit(key)}>{key === '⌫' ? <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 5 2 12l6 7h13a1 1 0 0 0 1-1V6a1 1 0 0 0-1-1H8Z"/><path d="m12 9 5 6m0-6-5 6"/></svg> : key}</button>}</For>
        </div>
        <label class="pos-ref-label" for="pos-reference">Reference <span>(optional)</span></label>
        <input id="pos-reference" class="pos-reference" type="text" maxlength="120" placeholder="e.g. a name or order" autocomplete="off" value={reference()} onInput={event => setReference(event.currentTarget.value)}/>
        <Show when={error()}><p class="pos-error" role="alert">{error()}</p></Show>
        <button class="pos-primary" disabled={/^0+$/.test(digits()) || busy()} onClick={() => void charge()}>{busy() ? 'Creating order…' : 'Charge'}</button>
      </main>
    </Show>
    <Show when={screen() === 'payment' ? activeId() : null} keyed>{_id => <main class="pos-payment">
      <div class="pos-order-heading"><div><h1>{label(active()!)}</h1><p>Order {shortId(active()!.order_id)}</p></div><Badge order={active()!} offline={offline()}/></div>
      <Show when={!active()!.cancelled_at && !terminal(active()!)} fallback={<section class="pos-outcome"><Badge order={active()!}/><p>{active()!.cancelled_at ? 'This order was cancelled. If money arrives at its address, review the payment in the order details.' : active()!.error || 'This order is finished.'}</p><p>{active()!.amount} {active()!.currency} · {active()!.xmr_amount} XMR</p></section>}>
        <div class="pos-checkout-card"><iframe title={`Payment and refund details for ${label(active()!)}`} src={`/pay/${encodeURIComponent(config.publicKey)}/orders/${encodeURIComponent(active()!.order_id)}?view=compact`}/></div>
      </Show>
      <Show when={error()}><p class="pos-error" role="alert">{error()}</p></Show>
      <Show when={!terminal(active()!)}>
        <button class="pos-primary" disabled={busy()} onClick={() => void backgroundOrder()}>Background order</button>
        <button class="pos-cancel" disabled={busy()} onClick={() => void cancelOrder()}>Cancel order</button>
        <p class="pos-action-hint">Keep the payment open while you serve the next customer.</p>
      </Show>
      <Show when={terminal(active()!)}><button class="pos-primary" onClick={resetKeypad}>New order</button></Show>
    </main>}</Show>
    <Show when={screen() === 'list'}><main class="pos-list" ref={el => { listElement = el; }}>
      <h1>Background orders</h1><p class="pos-list-subtitle">Keep track of every order while you serve the next customer.</p>
      <input type="search" aria-label="Search reference or order ID" placeholder="Search reference or order ID" value={search()} onInput={event => setSearch(event.currentTarget.value)}/>
      <div class="pos-tabs" role="tablist" aria-label="Order status">
        <button role="tab" aria-selected={tab() === 'active' ? 'true' : 'false'} class={tab() === 'active' ? 'selected' : ''} onClick={() => setTab('active')}>Active · {activeCount()}{nextOffset() < total() ? '+' : ''}</button>
        <button role="tab" aria-selected={tab() === 'finished' ? 'true' : 'false'} class={tab() === 'finished' ? 'selected' : ''} onClick={() => setTab('finished')}>Finished · {finishedCount()}{nextOffset() < total() ? '+' : ''}</button>
      </div>
      <Show when={loading()}><p>Loading orders…</p></Show>
      <Show when={error()}><p class="pos-error" role="alert">{error()} <button onClick={() => void refresh()}>Retry</button></p></Show>
      <Show when={!loading() && !error() && visible().length === 0}><p class="pos-empty">{search() ? 'No matching orders.' : `No ${tab()} background orders yet.`}</p></Show>
      <div class="pos-list-items"><For each={visible()}>{order => <article class="pos-order-card">
        <div class="pos-order-card-head"><div><h2>{label(order)}</h2><p>{order.merchant_order_id ? 'Reference · ' : ''}{shortId(order.order_id)} · created {new Date(order.created_at * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</p></div><Badge order={order} offline={offline() && !terminal(order)}/></div>
        <div class="pos-order-sum">{order.amount} <span>{order.currency}</span></div>
        <div class="pos-order-foot"><small>{order.cancelled_at && order.status !== 'pending' ? 'Payment activity after cancellation — review' : order.error || (order.status === 'partial' ? 'Waiting for remaining amount' : order.status === 'confirming' ? `${order.confirmations} of ${order.confirmations_required} confirmations` : statusName[stateOf(order)])}</small><button onClick={() => void openOrder(order)}>Open →</button></div>
      </article>}</For></div>
      <Show when={nextOffset() < total()}><button class="pos-load-more" onClick={() => void refresh(true)}>Load more orders</button></Show>
    </main></Show>
  </>;
}

render(() => <App/>, root);

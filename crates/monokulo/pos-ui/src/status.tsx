import { Show } from 'solid-js';

/** What the POS needs to know about an order to show its status. */
export type StatusSource = {
  status: string; confirmations: number; confirmations_required: number;
  error: string | null; cancelled_at: number | null;
};

/** The visual state: the engine's status, refined by what the POS knows. */
export function stateOf(order: StatusSource, offline = false): string {
  if (offline) return 'offline';
  if (order.cancelled_at) return 'cancelled';
  if (order.error?.includes('Double-spend')) return 'double-spend';
  if (order.status === 'confirming' && order.confirmations === 0) return 'unconfirmed';
  return order.status;
}

export const statusName: Record<string, string> = {
  pending: 'Awaiting payment', unconfirmed: 'Unconfirmed', confirming: 'Confirming', partial: 'Partially paid',
  paid: 'Paid', overpaid: 'Overpaid', expired: 'Expired', cancelled: 'Cancelled',
  'double-spend': 'Double spend', offline: 'Connection lost',
};

/** The confirming wedge: no wedge at zero confirmations (that's
 * "unconfirmed"), otherwise at least 20%, rising in 10% steps, full at the
 * threshold. */
export function progressPercent(order: StatusSource): number {
  if (order.confirmations_required <= 0) return 100;
  return Math.min(100, Math.max(20, Math.ceil((10 * order.confirmations) / order.confirmations_required) * 10));
}

/** Shared SVG definitions for the Monero coin artwork (sketch option E for
 * a partial payment: the larger fragment solid, a dashed outline for the
 * missing piece). Rendered once per page. */
export function StatusSymbols() {
  return <svg class="pos-symbol-defs" aria-hidden="true" xmlns="http://www.w3.org/2000/svg">
    <defs>
      <clipPath id="pos-coin-fragment"><path d="M0 0h21.5l-3.5 9.5 3.5 5.9L17.8 32H0Z"/></clipPath>
      <symbol id="pos-coin" viewBox="0 0 32 32">
        <path d="M4 12.5v6c0 5 5.4 9 12 9s12-4 12-9v-6" fill="currentColor" fill-opacity=".24" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>
        <path d="M8 22.5v3M16 24.5v3M24 22.5v3" fill="none" stroke="currentColor" stroke-opacity=".55" stroke-width="1"/>
        <ellipse cx="16" cy="12.5" rx="12" ry="9" fill="var(--pos-coin-face)" stroke="currentColor" stroke-width="1.6"/>
        <path d="M9.5 15.7V9.4l6.5 5 6.5-5v6.3" fill="none" class="pos-coin-mark" stroke-width="2.5" stroke-linecap="square"/>
        <path d="M9.5 15.7v1.2h13v-1.2" fill="none" stroke="currentColor" stroke-width="1"/>
      </symbol>
      <symbol id="pos-coin-partial" viewBox="0 0 32 32">
        <use href="#pos-coin" clip-path="url(#pos-coin-fragment)"/>
        <path d="M21.5 4 18 9.5 21.5 15.4 17.8 27" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
        <path d="M21.5 4.2C25.6 6 28 8.9 28 12.5v6c0 4.7-4.2 8.1-10.2 8.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-dasharray="2.2 2.2" stroke-linecap="round"/>
      </symbol>
      <symbol id="pos-coins-overpaid" viewBox="0 0 44 32">
        <use href="#pos-coin" x="0" y="3" width="30" height="29"/><use href="#pos-coin" x="13" y="0" width="30" height="29"/>
      </symbol>
    </defs>
  </svg>;
}

/** One status symbol, centred in a square box sized by the caller's CSS. */
export function StatusIcon(props: { order: StatusSource; offline?: boolean }) {
  const state = () => stateOf(props.order, props.offline);
  return <span class={['pos-icon', `pos-icon-${state()}`]} aria-hidden="true">
    <Show when={state() === 'pending'}><span class="pos-spinner"/></Show>
    <Show when={state() === 'unconfirmed'}><span class="pos-disc"/></Show>
    <Show when={state() === 'confirming'}><span class="pos-disc" style={{ '--progress': `${progressPercent(props.order)}%` }}/></Show>
    <Show when={state() === 'partial'}><svg viewBox="0 0 32 32"><use href="#pos-coin-partial"/></svg></Show>
    <Show when={state() === 'paid'}><svg viewBox="0 0 32 32"><use href="#pos-coin"/></svg></Show>
    <Show when={state() === 'overpaid'}><svg viewBox="0 0 44 32"><use href="#pos-coins-overpaid"/></svg></Show>
    <Show when={state() === 'expired'}>
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M6 3h12M6 21h12M7.5 3v3.5c0 2.6 4.5 4 4.5 5.5s-4.5 2.9-4.5 5.5V21M16.5 3v3.5c0 2.6-4.5 4-4.5 5.5s4.5 2.9 4.5 5.5V21"/>
        <path d="M8.5 20.2c.8-1.8 2.2-2.6 3.5-2.6s2.7.8 3.5 2.6Z" fill="currentColor" stroke="none"/>
      </svg>
    </Show>
    <Show when={state() === 'double-spend'}>
      <svg viewBox="0 0 24 24" fill="currentColor"><rect x="10.4" y="3.5" width="3.2" height="11" rx="1.2"/><circle cx="12" cy="19" r="1.9"/></svg>
    </Show>
    <Show when={state() === 'offline'}>
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
        <path d="M2.5 9a14 14 0 0 1 19 0M5.5 12.5a9.5 9.5 0 0 1 13 0M8.8 15.8a5 5 0 0 1 6.4 0"/><circle cx="12" cy="19.5" r="1.1" fill="currentColor" stroke="none"/><path d="M4 4l16 16"/>
      </svg>
    </Show>
    <Show when={state() === 'cancelled'}>
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M7 7l10 10M17 7 7 17"/></svg>
    </Show>
  </span>;
}

/** Words short enough for a small phone's heading ("Waiting"); the full
 * ones stay for screen readers. */
export const statusShortName: Record<string, string> = {
  pending: 'Waiting', unconfirmed: 'Seen', partial: 'Part paid', offline: 'Offline',
};

/** A status pill: symbol plus word (payment page and list). */
export function StatusBadge(props: { order: StatusSource; offline?: boolean }) {
  const state = () => stateOf(props.order, props.offline);
  const name = () => statusName[state()] || props.order.status;
  return <span class={['pos-badge', `state-${state()}`]}><StatusIcon order={props.order} offline={props.offline}/>
    <Show when={statusShortName[state()]} fallback={name()}>{short => <><span class="label-long">{name()}</span><span class="label-short" aria-hidden="true">{short()}</span></>}</Show>
  </span>;
}

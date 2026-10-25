import { StatusIcon as OrderIcon, Tag, type OrderState } from '@monokulo/ui/solid';
import STATUS_SYMBOLS from '@monokulo/ui/status/symbols.svg?raw';

/** What the POS needs to know about an order to show its status. */
export type StatusSource = {
  status: string; confirmations: number; confirmations_required: number;
  error: string | null;
};

/** The visual state: the engine's status, refined by what the POS knows. */
export function stateOf(order: StatusSource, offline = false): OrderState {
  if (offline) return 'offline';
  if (order.error?.includes('Double-spend')) return 'double-spend';
  if (order.status === 'confirming' && order.confirmations === 0) return 'unconfirmed';
  return order.status as OrderState;
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
  return <span class="pos-symbol-defs" aria-hidden="true" innerHTML={STATUS_SYMBOLS}/>;
}

/** One status symbol, centred in a square box sized by the caller's CSS:
 * the ui package's `StatusIcon`, with the order's state and its wedge. */
export function StatusIcon(props: { order: StatusSource; offline?: boolean }) {
  const state = () => stateOf(props.order, props.offline);
  return <OrderIcon state={state()} progress={state() === 'confirming' ? progressPercent(props.order) : undefined}/>;
}

/** Words short enough for a small phone's heading ("Waiting"); the full
 * ones stay for screen readers. */
export const statusShortName: Record<string, string> = {
  pending: 'Waiting', unconfirmed: 'Seen', partial: 'Part paid', offline: 'Offline',
};

/** An order's status as a `Tag`: symbol plus word (payment page and list),
 * the short word on a phone. */
export function StatusTag(props: { order: StatusSource; offline?: boolean }) {
  const state = () => stateOf(props.order, props.offline);
  return <Tag state={state()} short={statusShortName[state()]} data-pos="status"
    icon={<StatusIcon order={props.order} offline={props.offline}/>}>{statusName[state()] || props.order.status}</Tag>;
}

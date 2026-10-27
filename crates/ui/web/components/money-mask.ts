// How a MoneyInput's text is written (crates/ui/src/components/forms.rs):
// thousands grouped with a comma, a dot for decimals, the same on every
// page and in every locale, so what a merchant reads is what the server
// shows. The plain number (digits and a dot, `1250.5`) is what is stored
// and sent; nothing here rounds, so no digit someone typed or the server
// rendered is ever dropped without them seeing it.

export const GROUP = ',';
export const POINT = '.';

/** The digits of `text` before and after its point, and whether it has one. Every comma is grouping. */
function split(text: string): { whole: string; fraction: string; point: boolean } {
  let whole = '';
  let fraction = '';
  let point = false;
  for (const ch of text) {
    if (ch >= '0' && ch <= '9') {
      if (point) fraction += ch;
      else whole += ch;
    } else if (ch === POINT && !point) {
      point = true;
    }
  }
  return { whole: whole.replace(/^0+(?=\d)/, ''), fraction, point };
}

const group = (whole: string) => whole.replace(/\B(?=(\d{3})+(?!\d))/g, GROUP);

/**
 * `text` written out: grouped, and once done (`!typing`) a started
 * fraction filled to the currency's places when it has 2 or fewer (`12.5`
 * USD is `12.50`). Places past `decimals` are kept, never cut: the field
 * is then refused, not quietly changed.
 */
export function format(text: string, decimals: number, typing = false): string {
  let { whole, fraction, point } = split(text);
  if (whole === '' && !point) return '';
  if (whole === '') whole = '0';
  if (!typing) {
    // Cents read as cents (`12.50`); Monero's twelve places read shortest (`0.125`).
    if (decimals > 2) fraction = fraction.replace(/0+$/, '');
    else if (fraction.length > 0) fraction = fraction.length > decimals ? fraction.replace(/0+$/, '').padEnd(decimals, '0') : fraction.padEnd(decimals, '0');
  }
  const keepPoint = point && decimals > 0 && (typing || fraction !== '');
  return group(whole) + (keepPoint ? POINT + fraction : '');
}

/** The plain number `text` says (`1,250.50` is `1250.50`), or '' for none. */
export function plain(text: string): string {
  const { whole, fraction, point } = split(text);
  if (whole === '' && fraction === '') return '';
  return (whole || '0') + (point && fraction ? POINT + fraction : '');
}

/** One spelling per amount (`1,250.50` and `1250.5` are both `1250.5`): what a change is judged by. */
export function canonical(text: string): string {
  const [whole, fraction = ''] = plain(text).split(POINT);
  const trimmed = fraction.replace(/0+$/, '');
  return whole === undefined || whole === '' ? '' : whole + (trimmed ? POINT + trimmed : '');
}

/** How many places past the point `text` has. */
export function places(text: string): number {
  return split(text).fraction.length;
}

/** Whether plain `a` is more than plain `b`, exactly, with no float. */
export function above(a: string, b: string): boolean {
  const [aw = '', af = ''] = plain(a).split(POINT);
  const [bw = '', bf = ''] = plain(b).split(POINT);
  const width = Math.max(af.length, bf.length);
  const av = (aw.replace(/^0+/, '') || '0') + af.padEnd(width, '0');
  const bv = (bw.replace(/^0+/, '') || '0') + bf.padEnd(width, '0');
  const as = av.replace(/^0+(?=\d)/, '');
  const bs = bv.replace(/^0+(?=\d)/, '');
  return as.length !== bs.length ? as.length > bs.length : as > bs;
}

/** Digits and the point in `text` before `end`: what a caret is after. */
export function marksBefore(text: string, end: number): number {
  let count = 0;
  for (const ch of text.slice(0, end)) if ((ch >= '0' && ch <= '9') || ch === POINT) count++;
  return count;
}

/** Where a caret goes in `text` to be after `marks` digits and points. */
export function caretAfter(text: string, marks: number): number {
  let seen = 0;
  for (let i = 0; i < text.length; i++) {
    if (seen === marks) return i;
    const ch = text[i];
    if ((ch >= '0' && ch <= '9') || ch === POINT) seen++;
  }
  return text.length;
}

/**
 * What pasted `text` means, as a plain number, or `undefined` when it
 * can't be told: `1,250` is 1250 to some and 1.25 to others, and for
 * Monero that is a thousandfold difference, so it is asked about, not
 * guessed. With both marks the last one is the point (`1,250.50`,
 * `1.250,50`); a lone comma with 3 digits after it is the ambiguous case.
 */
export function readPasted(text: string): string | undefined {
  // A sign or an exponent is no amount to take apart: `-5` is not 5.
  if (/[+\-−]\s*\d|\d\s*[eE]\s*[+\-]?\d/.test(text)) return undefined;
  const kept = text.replace(/[^0-9.,]/g, '');
  const lastDot = kept.lastIndexOf('.');
  const lastComma = kept.lastIndexOf(',');
  const groupsOf = (body: string, mark: string) => {
    const parts = body.split(mark);
    return parts.length > 1 && /^[1-9]\d{0,2}$/.test(parts[0]) && parts.slice(1).every(p => /^\d{3}$/.test(p));
  };
  if (lastDot >= 0 && lastComma >= 0) {
    const pointAt = Math.max(lastDot, lastComma);
    const groupMark = pointAt === lastDot ? ',' : '.';
    const whole = kept.slice(0, pointAt);
    if (whole.includes(kept[pointAt]) || !(groupsOf(whole, groupMark) || /^\d*$/.test(whole))) return undefined;
    return plain(whole.split(groupMark).join('') + POINT + kept.slice(pointAt + 1));
  }
  for (const mark of [',', '.']) {
    const count = kept.split(mark).length - 1;
    if (count === 0) continue;
    if (count > 1) return groupsOf(kept, mark) ? plain(kept.split(mark).join('')) : undefined;
    const after = kept.slice(kept.indexOf(mark) + 1);
    const before = kept.slice(0, kept.indexOf(mark));
    // A lone dot is always the point; a lone comma with 3 after could be
    // either, unless what's before it can't start a grouped number (`0,125`).
    if (mark === ',' && /^\d{3}$/.test(after) && /^[1-9]\d{0,2}$/.test(before)) return undefined;
    return plain(kept.replace(mark, POINT));
  }
  return plain(kept);
}

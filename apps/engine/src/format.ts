// How the engine page writes its figures. Monokulo's Status page writes some
// of the same in Rust (views::scaling): a slow block, sizes, rates.

/** `x` to `digits` decimal places, as Rust's `{:.N}` writes it: an exact
 *  half goes to the even digit (12.5 is "12", 0.125 is "0.12"), where
 *  `toFixed` would round it up. Anything not exactly half way rounds to the
 *  nearer, the same either way. */
export function fixed(x: number, digits: number): string {
  // The number's exact decimal expansion: enough places for any double
  // these figures reach.
  const exact = Math.abs(x).toFixed(Math.min(100, digits + 60));
  const point = exact.indexOf('.');
  if (!/^50*$/.test(exact.slice(point + 1 + digits))) return x.toFixed(digits);
  const kept = exact.slice(0, digits ? point + 1 + digits : point);
  const sign = x < 0 ? '-' : '';
  if (Number(kept[kept.length - 1]) % 2 === 0) return sign + kept;
  return sign + (Number(kept) + 10 ** -digits).toFixed(digits);
}

/** `n` with thousands separators: "3,412,001". */
export function thousands(n: number): string {
  return String(Math.trunc(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

/** `n` and `noun`, plural unless one: "1 payment", "1,204 payments". */
export function plural(n: number, noun: string): string {
  return n === 1 ? `1 ${noun}` : `${thousands(n)} ${noun}s`;
}

/** "1 store", "3 stores". */
export const storesPhrase = (stores: number): string => plural(stores, 'store');

export function capitalised(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** The figure for something not known yet. */
export const DASH = '–';

/** "0.42s" under a second, "7.8s" over. */
export function seconds(ms: number): string {
  return ms < 1000 ? `${fixed(ms / 1000, 2)}s` : `${fixed(ms / 1000, 1)}s`;
}

/** "4ms", "1,204ms": the round card's times, whole milliseconds so its parts
 *  visibly add up to it. */
export const milliseconds = (ms: number): string => `${thousands(ms)}ms`;

/** A time as a lane writes it: "<1ms" for a part that took under a
 *  millisecond (times are whole milliseconds), never "0ms". */
export const laneTime = (ms: number): string => (ms === 0 ? '<1ms' : milliseconds(ms));

export const megabytes = (bytes: number): string => `${fixed(bytes / (1024 * 1024), 1)} MB`;

/** Decimal kilobytes, as block sizes are usually given: "41 kB". */
export const kilobytes = (bytes: number): string => `${thousands(Math.ceil(bytes / 1000))} kB`;

/** A count in at most four characters: "23", "1.2k", "12k", "1.2M". */
export function compact(n: number): string {
  if (n < 1_000) return String(n);
  if (n < 10_000) return `${fixed(n / 1000, 1)}k`;
  if (n < 1_000_000) return `${Math.floor(n / 1000)}k`;
  return `${fixed(n / 1_000_000, 1)}M`;
}

export const micros = (us: number): string => (us < 1000 ? `${us}µs` : `${fixed(us / 1000, 1)}ms`);

/** `x` to three places: enough to draw. */
export const rounded = (x: number): number => Math.round(x * 1000) / 1000;

/** `part` of `whole`, 0 to 1, to three places. */
export function fraction(part: number, whole: number): number {
  return whole === 0 ? 0 : rounded(Math.min(1, Math.max(0, part / whole)));
}

/** A size in decimal units, as sizes on a network are given: "412 MB",
 *  "1.8 MB", "13.0 kB", "900 B". */
export function bytes(n: number): string {
  let value: number, unit: string;
  if (n >= 1e9) [value, unit] = [n / 1e9, 'GB'];
  else if (n >= 1e6) [value, unit] = [n / 1e6, 'MB'];
  else if (n >= 1e3) [value, unit] = [n / 1e3, 'kB'];
  else return `${n} B`;
  return value >= 100 ? `${fixed(value, 0)} ${unit}` : `${fixed(value, 1)} ${unit}`;
}

/** A link's rate, from bytes a second: "3.1 Mbit/s", "480 kbit/s". */
export function rate(bytesPerSec: number): string {
  const bits = bytesPerSec * 8;
  if (bits >= 1e9) return `${fixed(bits / 1e9, 1)} Gbit/s`;
  if (bits >= 1e6) return `${fixed(bits / 1e6, 1)} Mbit/s`;
  return `${fixed(bits / 1e3, 0)} kbit/s`;
}

/** An elapsed time to the second: "2 m 10 s", "45 s", "1 h 5 m". */
export function duration(secs: number): string {
  const s = Math.max(0, Math.trunc(secs));
  const [h, m, rest] = [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60];
  if (h > 0) return `${h} h ${m} m`;
  if (m > 0) return `${m} m ${rest} s`;
  return `${rest} s`;
}

/** A rough length of time: "18 minutes", "40 seconds", "2 hours". */
export function durationRough(secs: number): string {
  const s = Math.max(0, Math.trunc(secs));
  const unit = (n: number, name: string) => `${n} ${name}${n === 1 ? '' : 's'}`;
  if (s >= 2 * 3600) return unit(Math.round(s / 3600), 'hour');
  if (s >= 90) return unit(Math.round(s / 60), 'minute');
  return unit(s, 'second');
}

/** A network's name as a word: "Mainnet". */
export const networkName = (network: string): string => capitalised(network);

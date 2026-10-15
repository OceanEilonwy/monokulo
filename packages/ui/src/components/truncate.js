/** Unicode code-point ranges, never slices of UTF-16 surrogate pairs.
 * Ranges are start:length; negative starts count from the end. Invalid or
 * overlapping ranges leave the full value intact rather than hide wrong data.
 */
export function truncateValue(text, mode, ranges = '', head = 8, tail = 8) {
  const chars = Array.from(text);
  if (mode === 'middle') {
    head = Number.isInteger(head) && head > 0 ? head : 8;
    tail = Number.isInteger(tail) && tail > 0 ? tail : 8;
    if (chars.length <= head + tail + 1) return text;
    return chars.slice(0, head).join('') + '…' + chars.slice(-tail).join('');
  }
  if (mode !== 'segments' || !ranges) return text;
  const selected = ranges.split(',').map(range => {
    if (!/^-?\d+:\d+$/.test(range.trim())) return null;
    let [start, length] = range.trim().split(':').map(Number);
    if (start < 0) start += chars.length;
    return start >= 0 && length > 0 && start + length <= chars.length ? [start, start + length] : null;
  });
  if (selected.some((range, i) => !range || (i && range[0] < selected[i - 1][1]))) return text;
  let end = 0;
  let result = '';
  for (const [start, stop] of selected) {
    if (start > end) result += '…';
    result += chars.slice(start, stop).join('');
    end = stop;
  }
  if (end < chars.length) result += '…';
  return result;
}

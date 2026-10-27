// How a MoneyInput writes, reads and pastes amounts (web/components/money-mask.ts).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { above, canonical, caretAfter, format, marksBefore, plain, readPasted } from '../web/components/money-mask.ts';

test('amounts are grouped with commas and keep a dot for decimals', () => {
  assert.equal(format('1250.5', 2), '1,250.50');
  assert.equal(format('1000000', 0), '1,000,000');
  assert.equal(format('0.125', 12), '0.125');
  assert.equal(format('0.125000000000', 12), '0.125');
  assert.equal(format('.5', 2), '0.50');
  assert.equal(format('0012', 2), '12');
  assert.equal(format('', 2), '');
});

test('while typing, a trailing point stays and no place is ever cut', () => {
  assert.equal(format('12.', 2, true), '12.');
  assert.equal(format('12.345', 2, true), '12.345');
  assert.equal(format('0.1234567890123', 12, true), '0.1234567890123');
  assert.equal(format('1250', 2, true), '1,250');
});

test('places the server had are never cut once typing is done', () => {
  // Shown as it is, so the field is refused rather than quietly changed.
  assert.equal(format('12.345', 2), '12.345');
});

test('the plain number is what is sent', () => {
  assert.equal(plain('1,250.50'), '1250.50');
  assert.equal(plain('0.125'), '0.125');
  assert.equal(plain('125,000,000,000'), '125000000000');
  assert.equal(plain(''), '');
});

test('a paste is read for what it means, or refused when it could be either', () => {
  assert.equal(readPasted('$1,250.50 USD'), '1250.50');
  assert.equal(readPasted('1.250,50 €'), '1250.50');
  assert.equal(readPasted('0,125'), '0.125');
  assert.equal(readPasted('12,5'), '12.5');
  assert.equal(readPasted('1,250,000'), '1250000');
  assert.equal(readPasted('0.125 XMR'), '0.125');
  // 1250 or 1.25: a thousandfold apart in Monero.
  assert.equal(readPasted('1,250'), undefined);
  assert.equal(readPasted('1,25,0'), undefined);
  // Signs and exponents are refused, not stripped.
  assert.equal(readPasted('-5.00'), undefined);
  assert.equal(readPasted('5e1'), undefined);
  assert.equal(readPasted('1,250 EUR'), undefined);
  assert.equal(readPasted('12.50 EUR'), '12.50');
});

test('one amount has one canonical spelling', () => {
  assert.equal(canonical('1,250.50'), canonical('1250.5'));
  assert.equal(canonical('12.50'), '12.5');
  assert.equal(canonical('100'), '100');
  assert.equal(canonical(''), '');
});

test('the caret stays after the same digits as grouping comes and goes', () => {
  // "1250|" written out as "1,250|".
  assert.equal(caretAfter('1,250', marksBefore('1250', 4)), 5);
  // "12|50" is "1,2|50".
  assert.equal(caretAfter('1,250', marksBefore('1250', 2)), 3);
});

test('amounts compare exactly, with no float', () => {
  assert.equal(above('1000000.000000000001', '1000000'), true);
  assert.equal(above('1000000', '1000000'), false);
  assert.equal(above('125000000000', '1000000'), true);
  assert.equal(above('0.125', '1000000'), false);
  assert.equal(above('9', '10'), false);
});

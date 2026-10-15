import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inspect, key, obsolete, unaccepted } from './lint.mjs';
import { truncateValue } from '../web/components/truncate.ts';

test('shared controls are required in HTML, JSX and Maud', () => {
  for (const [file, source] of [['x.html', '<button>Save</button>'], ['x.tsx', 'const x = <button>Save</button>;'], ['x.rs', 'button type="submit" { "Save" }']]) {
    assert.ok(inspect(file, source).some(f => f.rule === 'shared-control'));
  }
  assert.equal(inspect('x.html', '<button class="mk-btn">Save</button>').length, 0);
  assert.equal(inspect('x.tsx', 'const x = <input class="mk-input"/>;').length, 0);
});
test('new CSS drift and inaccessible icon controls fail', () => {
  const rules = inspect('app.css', '.mk-btn { color: #fff; font-size: 17px; }').map(f => f.rule);
  assert.ok(rules.includes('component-override') && rules.includes('color-token') && rules.includes('type-token'));
  assert.ok(inspect('x.html', '<button class="mk-btn" data-icon-only></button>').some(f => f.rule === 'icon-name'));
});
test('baseline permits existing debt, never copied or changed violations', () => {
  const [finding] = inspect('app.html', '<button>Save</button>');
  const baseline = { [key(finding)]: { count: 1 } };
  assert.equal(unaccepted([finding], baseline).length, 0);
  assert.equal(unaccepted([finding, finding], baseline).length, 1);
  assert.equal(unaccepted(inspect('app.html', '<button class="custom">Save</button>'), baseline).length, 1);
});
test('middle and range truncation preserve Unicode and reject invalid ranges', () => {
  assert.equal(truncateValue('😀abcdefghijk🚀', 'middle', '', 2, 2), '😀a…k🚀');
  assert.equal(truncateValue('abcdefghijklmnop', 'segments', '0:3,7:2,-3:3'), 'abc…hi…nop');
  for (const ranges of ['3:5,1:2', 'bad', '-50:3', '0:100', '0:4,2:5']) assert.equal(truncateValue('abcdef', 'segments', ranges), 'abcdef');
  assert.equal(truncateValue('short', 'middle', '', 8, 8), 'short');
});

test('a fixed finding left in the baseline is reported as obsolete', () => {
  const [finding] = inspect('app.html', '<button>Save</button>');
  const baseline = { [key(finding)]: { count: 2 }, 'gone.css:type-token:0000000000000000': { count: 1 } };
  assert.equal(obsolete([finding], baseline), 2);
  assert.equal(obsolete([finding, finding], { [key(finding)]: { count: 2 } }), 0);
});

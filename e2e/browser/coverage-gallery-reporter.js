// Collects the coverage stages (coverage-screenshot.js) into the published
// screenshot gallery: `images/`, `manifest.json` and `index.html` beside the
// coverage report. The stages' images are already in `images/`, saved there
// by the test; failed tests' screenshots are copied in here. More than one
// Playwright run can feed one gallery (the
// browser suite, then the real-binaries suite): each run adds its entries to
// the manifest already there and rewrites the page from all of them.
//
// The page files stages under the page or feature they show, and toggles
// at the top pick the size shown (Desktop, Tablet or Mobile, and Portrait or
// Landscape within Tablet and Mobile) and the theme (Light or Dark). It is
// radio buttons and CSS
// (`:has`), so it works opened straight from disk with no script.
//
// Options (the reporter tuple's second item): `required`, the groups this
// run must produce; `report`, the link to this run's Playwright report,
// relative to the gallery.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { guessGroup, gallery, photographs } = require('./coverage-screenshot.js');

const enabled = process.env.COVERAGE_SCREENSHOTS === '1' && process.env.COVERAGE_OUTPUT;
const stagenet = process.env.COVERAGE_PROFILE === 'stagenet';
const images = gallery && path.join(gallery, 'images');
const defaultReport = stagenet ? '../playwright-report/index.html' : '../browser/playwright-report/index.html';
const escapeHtml = value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;')
  .replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

/** Page and feature groups, in the order the gallery shows them; others
 * follow alphabetically under their own name. */
const GROUPS = [
  ['site', 'Dashboard'],
  ['hosted-payment', 'Hosted payment page'],
  ['checkout', 'Checkout'],
  ['challenge', 'Challenge'],
  ['pos', 'POS'],
  ['pos-timeline', 'POS session timeline'],
  ['store-settings', 'Store settings'],
  ['logs', 'Logs'],
  ['admin-settings', 'Admin settings'],
];
const SHAPES = ['desktop', 'tablet-portrait', 'tablet-landscape', 'mobile-portrait', 'mobile-landscape', 'small-portrait'];
const SHAPE_LABELS = {
  desktop: 'Desktop', 'tablet-portrait': 'Tablet, portrait', 'tablet-landscape': 'Tablet, landscape',
  'mobile-portrait': 'Mobile, portrait', 'mobile-landscape': 'Mobile, landscape', 'small-portrait': 'Small phone (320px), portrait',
  'as-is': 'at the size the test set',
};
const THEMES = ['light', 'dark'];
// Captures made before shapes had device names.
const OLD_SHAPES = { portrait: 'mobile-portrait', landscape: 'mobile-landscape' };

/** `<group>/<stage>@<shape>[+<theme>]` (coverage-screenshot.js), or the
 * older `<stage>-<shape>`. Without a theme a capture is light. */
function parseStage(name, guessedGroup) {
  const at = name.lastIndexOf('@');
  if (at > 0) {
    const [groupPart, stage] = name.slice(0, at).includes('/') ? name.slice(0, at).split('/') : [guessedGroup, name.slice(0, at)];
    const [shape, theme = 'light'] = name.slice(at + 1).split('+');
    return { group: groupPart || guessedGroup, stage, shape, theme };
  }
  for (const shape of [...SHAPES, ...Object.keys(OLD_SHAPES)]) {
    if (name.endsWith(`-${shape}`)) return { group: guessedGroup, stage: name.slice(0, -shape.length - 1), shape: OLD_SHAPES[shape] || shape, theme: 'light' };
  }
  return { group: guessedGroup, stage: name, shape: 'element', theme: 'light' };
}

function groupLabel(group) {
  const known = GROUPS.find(([id]) => id === group);
  return known ? known[1] : group;
}

function groupOrder(a, b) {
  const index = group => { const i = GROUPS.findIndex(([id]) => id === group); return i < 0 ? GROUPS.length : i; };
  return index(a) - index(b) || a.localeCompare(b);
}

const STYLE = `
:root { --ink: #17212b; --muted: #52606d; --line: #ccd3db; --bg: #fff; --card: #fff; --well: #eef1f4; --link: #164e8a; --accent: #164e8a; --accent-ink: #fff; }
@media (prefers-color-scheme: dark) {
  :root { --ink: #e6ebf0; --muted: #9aa7b4; --line: #34404c; --bg: #12181e; --card: #182029; --well: #0d1216; --link: #8cb8ea; --accent: #8cb8ea; --accent-ink: #0d1216; }
}
* { box-sizing: border-box; }
body { font: 16px/1.5 system-ui, sans-serif; max-width: 1200px; margin: 0 auto; padding: 1.5rem 16px 3rem; color: var(--ink); background: var(--bg); }
a { color: var(--link); }
h1 { margin: 0 0 .25rem; }
.toolbar { position: sticky; top: 0; z-index: 1; background: var(--bg); padding: .75rem 0; border-bottom: 1px solid var(--line); display: flex; flex-wrap: wrap; gap: .75rem 1.5rem; align-items: center; }
.toolbar fieldset { border: 0; margin: 0; padding: 0; display: flex; gap: .25rem; align-items: center; }
.toolbar legend { float: left; margin-right: .5rem; font-size: .85rem; color: var(--muted); }
.toolbar label { cursor: pointer; }
.toolbar input { position: absolute; opacity: 0; pointer-events: none; }
.toolbar label span { display: inline-block; padding: .3rem .8rem; border: 1px solid var(--line); border-radius: 999px; }
.toolbar input:checked + span { background: var(--accent); color: var(--accent-ink); border-color: var(--accent); }
.toolbar input:focus-visible + span { outline: 2px solid var(--accent); outline-offset: 2px; }
body:has(#device-desktop:checked) .orientation { display: none; }
.jump { display: flex; flex-wrap: wrap; gap: .25rem 1rem; margin: .75rem 0 0; padding: 0; list-style: none; font-size: .9rem; }
section { margin-top: 2rem; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 1rem; }
.card { border: 1px solid var(--line); border-radius: 8px; padding: .7rem; background: var(--card); min-width: 0; }
.card figure { display: none; margin: 0; }
.card figure[data-shape="element"], .card figure[data-shape="failure"] { display: block; }
.card img { width: 100%; height: 200px; object-fit: contain; object-position: top; background: var(--well); display: block; }
.card p { margin: .3rem 0; overflow-wrap: anywhere; }
.card .missing { display: none; height: 200px; margin: 0; place-items: center; text-align: center; background: var(--well); color: var(--muted); font-size: .9rem; padding: 1rem; }
small { color: var(--muted); }
`;

/** The rules showing each card's image for the chosen size and theme, or
 * saying the stage wasn't captured that way. */
function shapeRules() {
  return SHAPES.flatMap(shape => THEMES.map(theme => {
    const [device, orientation] = shape.split('-');
    const chosen = (orientation ? `body:has(#device-${device}:checked):has(#orientation-${orientation}:checked)` : `body:has(#device-${device}:checked)`)
      + `:has(#theme-${theme}:checked)`;
    const figure = `figure[data-shape="${shape}"][data-theme="${theme}"]`;
    return `${chosen} .card ${figure} { display: block; }\n`
      + `${chosen} .card.sized:not(:has(${figure})) .missing { display: grid; }`;
  })).concat(THEMES.map(theme => // At the test's own size, whichever size is chosen.
    `body:has(#theme-${theme}:checked) .card figure[data-shape="as-is"][data-theme="${theme}"] { display: block; }`)).join('\n');
}

function page(entries) {
  const groups = [...new Set(entries.map(e => e.group))].sort(groupOrder);
  let html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>UI stages</title><style>${STYLE}\n${shapeRules()}</style></head><body>`;
  html += '<h1>UI stages</h1><p><a href="../index.html">Coverage summary</a>';
  for (const report of [...new Set(entries.map(e => e.report))]) html += ` · <a href="${escapeHtml(report)}">Playwright test report${report.includes('real') ? ' (real binaries)' : ''}</a>`;
  html += '</p>';
  html += '<form class="toolbar" aria-label="Screenshot size and theme">'
    + '<fieldset><legend>Device</legend>'
    + '<label><input type="radio" name="device" id="device-desktop" checked><span>Desktop</span></label>'
    + '<label><input type="radio" name="device" id="device-tablet"><span>Tablet</span></label>'
    + '<label><input type="radio" name="device" id="device-mobile"><span>Mobile</span></label>'
    + '<label><input type="radio" name="device" id="device-small"><span>Small phone</span></label>'
    + '</fieldset><fieldset class="orientation"><legend>Orientation</legend>'
    + '<label><input type="radio" name="orientation" id="orientation-portrait" checked><span>Portrait</span></label>'
    + '<label><input type="radio" name="orientation" id="orientation-landscape"><span>Landscape</span></label>'
    + '</fieldset><fieldset><legend>Theme</legend>'
    + '<label><input type="radio" name="theme" id="theme-light" checked><span>Light</span></label>'
    + '<label><input type="radio" name="theme" id="theme-dark"><span>Dark</span></label>'
    + '</fieldset></form>';
  html += `<ul class="jump">${groups.map(g => `<li><a href="#group-${escapeHtml(g)}">${escapeHtml(groupLabel(g))}</a></li>`).join('')}</ul>`;
  for (const group of groups) {
    html += `<section id="group-${escapeHtml(group)}"><h2>${escapeHtml(groupLabel(group))}</h2><div class="grid">`;
    // One card per stage of a test, holding each size it was captured at.
    const cards = new Map();
    for (const entry of entries.filter(e => e.group === group)) {
      const key = `${entry.test_id}:${entry.retry}:${entry.stage}`;
      if (!cards.has(key)) cards.set(key, []);
      cards.get(key).push(entry);
    }
    for (const shots of cards.values()) {
      const first = shots[0];
      const sized = shots.some(s => SHAPES.includes(s.shape));
      const only = SHAPES.flatMap(shape => THEMES.filter(theme => shots.some(s => s.shape === shape && (s.theme || 'light') === theme))
        .map(theme => `${SHAPE_LABELS[shape]} (${theme})`));
      html += `<article class="card${sized ? ' sized' : ''}">`;
      for (const shot of shots) {
        const theme = shot.theme || 'light';
        html += `<figure data-shape="${escapeHtml(shot.shape)}" data-theme="${escapeHtml(theme)}"><a href="${escapeHtml(shot.image)}"><img src="${escapeHtml(shot.image)}" alt="${escapeHtml(`${first.stage}, ${SHAPE_LABELS[shot.shape] || shot.shape}, ${theme}`)}" loading="lazy"></a></figure>`;
      }
      if (sized) html += `<p class="missing">Not captured at this size.<br>Captured: ${escapeHtml(only.join(', '))}</p>`;
      html += `<p><strong>${escapeHtml(first.stage)}</strong></p><p>${escapeHtml(first.test)}</p>`
        + `<p><a href="${escapeHtml(first.report)}#?testId=${encodeURIComponent(first.test_id)}">Test result</a></p>`
        + `<small>Retry ${first.retry}; ${escapeHtml(first.status)}</small></article>`;
    }
    html += '</div></section>';
  }
  return `${html}</body></html>`;
}

class CoverageGalleryReporter {
  constructor(options = {}) {
    this.entries = [];
    this.required = options.required || (stagenet ? ['pos'] : ['checkout', 'pos', 'challenge']);
    this.report = options.report || defaultReport;
  }
  onBegin() { if (enabled) fs.mkdirSync(images, { recursive: true }); }
  onTestEnd(test, result) {
    // Chromium's tests only, failures included: the gallery is one browser's.
    const project = test.parent.project();
    if (!enabled || (project && !photographs(project))) return;
    const guessed = guessGroup(test.location.file);
    let sequence = 0;
    for (const attachment of result.attachments) {
      let parsed, image;
      if (attachment.name.startsWith('coverage-stage:') && attachment.contentType === 'text/plain') {
        // Already in `images/`: the attachment names the file.
        parsed = parseStage(attachment.name.slice('coverage-stage:'.length), guessed);
        image = attachment.body.toString();
      } else if (attachment.name === 'screenshot' && attachment.contentType === 'image/png') {
        // Playwright's own screenshot of a failed test.
        parsed = { group: guessed, stage: 'failure', shape: 'failure', theme: 'light' };
        const id = crypto.createHash('sha256').update(`${test.id}:${result.retry}:${result.workerIndex}:${sequence}:failure`)
          .digest('hex').slice(0, 16);
        image = `images/${guessed}-failure-r${result.retry}-${id}.png`;
        fs.writeFileSync(path.join(gallery, image), attachment.body || fs.readFileSync(attachment.path));
      } else continue;
      const { group, stage, shape, theme } = parsed;
      this.entries.push({ group, test: test.title, test_id: test.id, retry: result.retry, worker: result.workerIndex,
        stage, shape, theme, sequence, status: result.status, image, report: this.report });
      sequence++;
    }
  }
  onEnd(result) {
    if (!enabled) return result;
    const manifestPath = path.join(gallery, 'manifest.json');
    const earlier = fs.existsSync(manifestPath) ? JSON.parse(fs.readFileSync(manifestPath, 'utf8')) : [];
    const mine = new Set(this.entries.map(e => e.image));
    const entries = [...earlier.filter(e => !mine.has(e.image)), ...this.entries];
    entries.sort((a, b) => groupOrder(a.group, b.group) || a.test.localeCompare(b.test)
      || a.retry - b.retry || a.sequence - b.sequence);
    fs.writeFileSync(manifestPath, JSON.stringify(entries, null, 2));
    fs.writeFileSync(path.join(gallery, 'index.html'), page(entries));
    const groups = new Set(this.entries.filter(e => e.stage !== 'failure').map(e => e.group));
    if (!this.required.every(group => groups.has(group))) {
      console.error(`coverage screenshots missing required stage groups: ${this.required.filter(g => !groups.has(g)).join(', ')}`);
      return { status: 'failed' };
    }
    return result;
  }
}

module.exports = CoverageGalleryReporter;

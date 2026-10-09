// The quality report's enhancements (cargo xtask pages build renders the pages).
// Every page reads fine without this: it adds the tests' and screens' filters
// and the screenshot viewer. It never writes markup from strings: text goes in
// through textContent and attributes through setAttribute.
'use strict';

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const int = n => n.toLocaleString('en-US');
const plural = (n, one, many) => `${int(n)} ${n === 1 ? one : many}`;
// As the build writes durations (xtask/src/pages/format.rs).
const duration = s => s >= 3600 ? (s / 3600).toFixed(1) + ' h' : s >= 60 ? Math.round(s / 60) + ' min' : s >= 1 ? s.toFixed(1) + ' s' : Math.round(s * 1000) + ' ms';
const idle = fn => (window.requestIdleCallback || (f => setTimeout(f, 300)))(fn);

/** An element with attributes and children (strings become text). */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v == null) continue;
    el.setAttribute(k, v === true ? '' : String(v));
  }
  el.append(...children.flat().filter(c => c != null && c !== false));
  return el;
}

/** One button pressed among a group of toggles. */
function press(buttons, pressed) {
  buttons.forEach(b => b.setAttribute('aria-pressed', String(b === pressed)));
}

for (const el of $$('[data-filters]')) el.hidden = false;

// ---------- all tests ----------
const tbox = $('#tbox');
if (tbox) {
  const state = {q: '', suite: '', area: ''};
  const rows = $$('li[data-suite]', tbox).map(li => {
    const group = li.closest('details[data-group]');
    return {
      li, group,
      area: li.closest('details[data-area]'),
      suite: li.dataset.suite,
      secs: Number(li.dataset.secs) || 0,
      text: [$('.label', li)?.textContent, $('.raw', li)?.textContent, $('.nm', group)?.textContent].join(' ').toLowerCase(),
    };
  });
  const areas = $$('details[data-area]', tbox);
  const count = $('#tcount');
  const none = $('#tnone');

  // Writes a summary's test count and time.
  const summarise = (details, shown) => {
    const summary = details.querySelector(':scope > summary');
    $('.cnt', summary).textContent = int(shown.length);
    $('.tim', summary).textContent = duration(shown.reduce((s, r) => s + r.secs, 0));
  };

  function filter() {
    const q = state.q.trim().toLowerCase().replace(/\s+/g, ' ');
    const words = [q, q.replace(/ /g, '_')];
    const shown = rows.filter(r => {
      const match = (!state.suite || r.suite === state.suite) && (!state.area || r.area.dataset.area === state.area)
        && (!q || words.some(w => r.text.includes(w)));
      r.li.hidden = !match;
      return match;
    });
    const narrowed = Boolean(q || state.area);
    for (const area of areas) {
      const inArea = shown.filter(r => r.area === area);
      area.hidden = !inArea.length;
      summarise(area, inArea);
      const groups = $$('details[data-group]', area);
      let visible = 0;
      for (const group of groups) {
        const inGroup = inArea.filter(r => r.group === group);
        group.hidden = !inGroup.length;
        if (inGroup.length) visible++;
        summarise(group, inGroup);
      }
      $('[data-groups]', area).textContent = plural(visible, 'group', 'groups');
      if (narrowed && inArea.length) {
        area.open = true;
        if (visible <= 6) groups.forEach(g => { if (!g.hidden) g.open = true; });
      }
    }
    count.textContent = `${plural(shown.length, 'test', 'tests')} · ${duration(shown.reduce((s, r) => s + r.secs, 0))} in total`;
    none.hidden = shown.length > 0;
  }

  $('#q').addEventListener('input', e => { state.q = e.target.value; filter(); });
  const suites = $$('[data-suite]', $('[data-filters]'));
  suites.forEach(b => b.addEventListener('click', () => { state.suite = b.dataset.suite; press(suites, b); filter(); }));
  const chips = $$('button[data-area]');
  const pickArea = id => {
    const chip = chips.find(c => c.dataset.area === id);
    if (!chip) return;
    state.area = id;
    press(chips, chip);
    filter();
  };
  chips.forEach(b => b.addEventListener('click', () => {
    if (location.hash) history.replaceState(null, '', location.pathname + location.search);
    pickArea(b.dataset.area);
  }));
  $('#expand-all').addEventListener('click', () => $$('details', tbox).forEach(d => { d.open = true; }));
  $('#collapse-all').addEventListener('click', () => $$('details', tbox).forEach(d => { d.open = false; }));
  // The front page's areas link here as #area-<id>.
  const fromHash = () => { const m = /^#area-([a-z]+)$/.exec(location.hash); if (m) pickArea(m[1]); };
  window.addEventListener('hashchange', fromHash);
  areas.forEach(a => { a.open = false; });
  fromHash();
}

// ---------- screens ----------
const gallery = $('#gallery');
if (gallery) {
  const SCREENS = JSON.parse($('#screens-data').textContent);
  const PHONE = 'mobile-portrait';
  const state = {q: '', group: ''};
  const cards = $$('.gcard', gallery).map(card => ({card, screen: SCREENS[Number(card.dataset.screen)], section: card.closest('.gsec')}));
  const sections = $$('.gsec', gallery);
  const none = $('#gnone');

  // The theme the gallery shows when it follows the reader's scheme.
  const theme = () => {
    const t = gallery.dataset.theme;
    return t === 'auto' ? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') : t;
  };
  // The shape a card shows: its phone shot when the phone view is on and it has one, else its first.
  const shapeIndex = (s, key) => Math.max(0, s.shapes.findIndex(x => x.shape === key));
  const cardShape = s => gallery.dataset.device === 'phone' ? shapeIndex(s, PHONE) : 0;

  function filter() {
    const q = state.q.trim().toLowerCase();
    let shown = 0;
    for (const section of sections) {
      const inSection = cards.filter(c => c.section === section);
      let visible = 0;
      for (const {card} of inSection) {
        const match = (!state.group || section.dataset.group === state.group) && (!q || card.dataset.q.includes(q));
        card.hidden = !match;
        if (match) visible++;
      }
      section.hidden = !visible;
      $('h3 span', section).textContent = plural(visible, 'screen', 'screens');
      shown += visible;
    }
    none.hidden = shown > 0;
  }
  $('#gq').addEventListener('input', e => { state.q = e.target.value; filter(); });
  const devices = $$('button[data-device]');
  devices.forEach(b => b.addEventListener('click', () => { gallery.dataset.device = b.dataset.device; press(devices, b); }));
  const themes = $$('button[data-theme]');
  themes.forEach(b => b.addEventListener('click', () => { gallery.dataset.theme = b.dataset.theme; press(themes, b); }));
  const groups = $$('button[data-group]');
  groups.forEach(b => b.addEventListener('click', () => { state.group = b.dataset.group; press(groups, b); filter(); }));

  // Full-size images: fetched once, decoded off the main thread, remembered for the visit.
  const FETCHED = new Map(), READY = new Set();
  function prefetch(url) {
    if (!url) return Promise.resolve();
    if (!FETCHED.has(url)) {
      const im = new Image();
      im.decoding = 'async';
      im.src = url;
      FETCHED.set(url, im.decode().then(() => READY.add(url), () => FETCHED.delete(url)));
    }
    return FETCHED.get(url);
  }
  const themesOf = shape => ['light', 'dark'].filter(t => shape[t]);
  const warm = (s, index, wanted = ['light', 'dark']) => {
    const shape = s.shapes[index];
    if (shape) wanted.forEach(t => prefetch(shape[t]?.full));
  };

  for (const {card, screen} of cards) {
    const ready = () => warm(screen, cardShape(screen));
    card.addEventListener('pointerenter', ready);
    card.addEventListener('focus', ready);
    card.addEventListener('touchstart', ready, {passive: true});
    card.addEventListener('click', e => {
      if (e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
      e.preventDefault();
      openViewer(card);
    });
  }
  // the first few cards are the likeliest clicks: fetch them while the browser is idle
  idle(() => cards.slice(0, 4).forEach(({screen}) => warm(screen, 0, [theme()])));

  const dialog = $('#viewer');
  // `focus` names the control a redraw came from, so it keeps the focus (and the arrow keys keep working).
  const view = {card: null, shape: 0, theme: 'both', focus: null};
  const visibleCards = () => cards.filter(c => !c.card.hidden && !c.section.hidden).map(c => c.card);

  function openViewer(card) {
    view.card = card;
    view.shape = cardShape(SCREENS[Number(card.dataset.screen)]);
    drawViewer();
    if (!dialog.open) dialog.showModal();
  }

  function figure(s, shape, t) {
    const im = shape[t];
    const ready = READY.has(im.full);
    const img = h('img', {class: ready ? null : 'pending', src: ready ? im.full : im.thumb, width: im.fw, height: im.fh, alt: `${s.title}, ${shape.label}, ${t}`});
    const loading = ready ? null : h('span', {class: 'ld'}, ' · loading full size');
    if (!ready) prefetch(im.full).then(() => {
      if (!img.isConnected || !READY.has(im.full)) return;
      img.src = im.full;
      img.classList.remove('pending');
      loading.remove();
    });
    return h('figure', {}, img, h('figcaption', {}, `${shape.label} · ${t} · ${im.fw}×${im.fh}`, loading));
  }

  function drawViewer() {
    const list = visibleCards();
    const i = list.indexOf(view.card);
    const s = SCREENS[Number(view.card.dataset.screen)];
    const shape = s.shapes[view.shape] || s.shapes[0];
    const wanted = view.theme === 'both' ? ['light', 'dark'] : [view.theme];
    // A shape taken in one theme only shows that one.
    const shown = wanted.filter(t => shape[t]).length ? wanted.filter(t => shape[t]) : themesOf(shape);
    const clicked = (b, onClick) => { b.addEventListener('click', () => { view.focus = b.id; onClick(); }); return b; };
    const button = (label, attrs, onClick) => clicked(h('button', {type: 'button', class: 'btn', ...attrs}, label), onClick);
    const toggle = (id, label, pressed, onClick) => clicked(h('button', {type: 'button', id, 'aria-pressed': String(pressed)}, label), onClick);
    const go = j => { if (list[j]) openViewer(list[j]); };
    const full = shape[shown[0]]?.full;
    dialog.replaceChildren(h('div', {class: 'vbox'},
      h('div', {class: 'vtop'},
        h('div', {}, h('h3', {id: 'v-title'}, s.title), s.test ? h('p', {}, `Test: “${s.test}”`) : null),
        h('div', {class: 'grp'},
          button('←', {id: 'v-prev', 'aria-label': 'Previous screen', disabled: i <= 0}, () => go(i - 1)),
          button('→', {id: 'v-next', 'aria-label': 'Next screen', disabled: i >= list.length - 1}, () => go(i + 1)),
          button('Close', {id: 'v-close'}, () => dialog.close()))),
      h('div', {class: 'vtools'},
        h('div', {class: 'seg', role: 'group', 'aria-label': 'Shape'},
          s.shapes.map((sh, j) => toggle(`v-shape-${j}`, sh.label, j === view.shape, () => { view.shape = j; drawViewer(); }))),
        h('div', {class: 'seg', role: 'group', 'aria-label': 'Theme'},
          [['light', 'Light'], ['dark', 'Dark'], ['both', 'Side by side']].map(([t, label]) => toggle(`v-theme-${t}`, label, view.theme === t, () => { view.theme = t; drawViewer(); })))),
      h('div', {class: 'vimg'}, shown.map(t => figure(s, shape, t))),
      h('div', {class: 'vfoot'},
        h('span', {}, s.passed ? h('span', {class: 'dot'}) : null, s.passed ? ' Passed' : (s.status || 'No result'), ` · ${i + 1} of ${list.length} · ← → to move, Esc to close`),
        h('span', {},
          s.report ? h('a', {href: s.report, target: '_blank', rel: 'noopener'}, 'Open in the Playwright report') : null,
          s.report && full ? ' · ' : null,
          full ? h('a', {href: full, target: '_blank', rel: 'noopener'}, 'Full-size image') : null))));
    if (view.focus) {
      // A control that's now disabled (← on the first screen) hands the focus to Close.
      const target = $('#' + view.focus, dialog);
      (target && !target.disabled ? target : $('#v-close', dialog)).focus();
      view.focus = null;
    }
    // then warm what's likely next: the screens either side, and this screen's other shapes
    idle(() => {
      for (const n of [list[i + 1], list[i - 1]]) {
        if (!n) continue;
        const next = SCREENS[Number(n.dataset.screen)];
        warm(next, shapeIndex(next, shape.shape), shown);
      }
      s.shapes.forEach((_, j) => { if (j !== view.shape) warm(s, j, shown); });
    });
  }
  dialog.addEventListener('keydown', e => {
    if (e.key === 'ArrowLeft') $('#v-prev', dialog)?.click();
    if (e.key === 'ArrowRight') $('#v-next', dialog)?.click();
  });
  dialog.addEventListener('click', e => { if (e.target === dialog) dialog.close(); });
}

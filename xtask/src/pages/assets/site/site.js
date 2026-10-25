// The site's script, on every page (xtask/src/pages/site_assets.rs): what
// only a script adds to pages that read fully without one.
//
// - Search: the bar's button (hidden until now), `/` or Ctrl+K open a
//   dialog that finds guides, their sections, the API's resources and
//   operations, and problems, from the indexes the builds write at
//   search/{part}.json (xtask/src/pages/shell.rs); arrows move, Enter
//   opens, Escape closes.
// - On a page in the shell: the section being read marked in "On this
//   page", the current page kept in sight in the map, and the map's menu
//   (a checkbox, on a narrow screen) closed by Escape or a link chosen.
// - On the router's install page: one OpenWrt version at a time, picked
//   from the dropdown it shows. Without a script every version's blocks
//   show, each under its own label.
(() => {
  const script = document.currentScript;
  // The script is in the site's assets/, one folder below its root.
  const root = new URL("../", script.src);
  const parts = (script.dataset.search || "").split(" ").filter(Boolean);

  /* ---------- search ---------- */
  const buttons = [...document.querySelectorAll(".site-nav-search")];
  let dialog, input, list, entries, hits = [], selected = 0;

  const load = () => entries ??= Promise.all(parts.map(part =>
    fetch(new URL(`search/${part}.json`, root))
      .then(r => (r.ok ? r.json() : []))
      .catch(() => []),
  )).then(lists => lists.flat());

  function build() {
    dialog = document.createElement("dialog");
    dialog.className = "search-dialog";
    dialog.setAttribute("aria-label", "Search the docs and the API");
    const box = document.createElement("div");
    box.className = "search-box";
    input = document.createElement("input");
    Object.assign(input, { type: "text", placeholder: "Search guides, endpoints and problems", autocomplete: "off", spellcheck: false });
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-label", "Search");
    input.setAttribute("aria-controls", "search-results");
    input.setAttribute("aria-expanded", "true");
    input.setAttribute("aria-autocomplete", "list");
    list = document.createElement("ul");
    list.id = "search-results";
    list.setAttribute("role", "listbox");
    list.setAttribute("aria-label", "Results");
    const keys = document.createElement("p");
    keys.className = "search-keys";
    keys.innerHTML = "<span><kbd>↑</kbd> <kbd>↓</kbd> move</span><span><kbd>Enter</kbd> open</span><span><kbd>Esc</kbd> close</span>";
    box.append(input, list, keys);
    dialog.append(box);
    document.body.append(dialog);
    // A click on the backdrop (the dialog itself, outside its box) closes it.
    dialog.addEventListener("click", e => { if (e.target === dialog) dialog.close(); });
    input.addEventListener("input", () => { selected = 0; draw(); });
    input.addEventListener("keydown", e => {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        selected = Math.max(0, Math.min(hits.length - 1, selected + (e.key === "ArrowDown" ? 1 : -1)));
        mark();
      } else if (e.key === "Enter" && hits[selected]) {
        e.preventDefault();
        go(hits[selected]);
      }
    });
    list.addEventListener("click", e => {
      const a = e.target.closest("a");
      if (a) { e.preventDefault(); go(hits[+a.dataset.i]); }
    });
  }

  // The entries every word is in, those whose title has the most of them
  // first, then pages, resources, endpoints and problems before sections:
  // what a reader names before the sections that mention it.
  function find(all, query) {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return all.filter(e => e.kind === "Guide");
    const scored = [];
    for (const e of all) {
      const title = e.title.toLowerCase();
      const text = `${title} ${e.sub} ${e.kind} ${e.method || ""}`.toLowerCase();
      if (!words.every(w => text.includes(w))) continue;
      const score = words.filter(w => title.includes(w)).length * 2 + (title.startsWith(words[0]) ? 1 : 0) + (e.kind === "Section" ? 0 : 1);
      scored.push([score, e]);
    }
    return scored.sort((a, b) => b[0] - a[0]).map(([, e]) => e);
  }

  async function draw() {
    const query = input.value;
    const all = await load();
    if (query !== input.value) return;
    hits = find(all, query).slice(0, 20);
    list.replaceChildren(...hits.map((hit, i) => {
      const li = document.createElement("li");
      li.setAttribute("role", "none");
      const a = document.createElement("a");
      a.href = new URL(hit.href, root);
      a.dataset.i = i;
      a.id = `search-hit-${i}`;
      a.setAttribute("role", "option");
      if (hit.method) {
        const m = document.createElement("span");
        m.className = "method";
        m.dataset.method = hit.method;
        m.textContent = hit.method;
        a.append(m);
      }
      const text = document.createElement("span");
      text.className = "hit";
      const title = document.createElement("b");
      title.textContent = hit.title;
      const sub = document.createElement("span");
      sub.textContent = hit.sub;
      text.append(title, sub);
      const kind = document.createElement("span");
      kind.className = "tag";
      kind.textContent = hit.kind;
      a.append(text, kind);
      li.append(a);
      return li;
    }));
    if (!hits.length) {
      const li = document.createElement("li");
      li.className = "search-none";
      li.setAttribute("role", "none");
      li.textContent = all.length ? `Nothing matches “${query}”.` : "Search isn't available here.";
      list.append(li);
    }
    mark();
  }

  function mark() {
    list.querySelectorAll("a").forEach((a, i) => a.setAttribute("aria-selected", i === selected));
    const current = list.querySelector(`#search-hit-${selected}`);
    if (current) {
      input.setAttribute("aria-activedescendant", current.id);
      current.scrollIntoView({ block: "nearest" });
    } else {
      input.removeAttribute("aria-activedescendant");
    }
  }

  function go(hit) {
    dialog.close();
    location.href = new URL(hit.href, root);
  }

  function open() {
    if (!dialog) build();
    if (dialog.open) return;
    input.value = "";
    selected = 0;
    dialog.showModal();
    input.focus();
    draw();
  }

  if (buttons.length && parts.length) {
    for (const button of buttons) {
      button.hidden = false;
      button.addEventListener("click", open);
    }
    document.addEventListener("keydown", e => {
      const typing = e.target.closest?.("input, textarea, select, [contenteditable]");
      if ((e.key === "/" && !typing && !e.ctrlKey && !e.metaKey && !e.altKey) || ((e.key || "").toLowerCase() === "k" && (e.ctrlKey || e.metaKey))) {
        e.preventDefault();
        open();
      }
    });
  }

  /* ---------- the shell ---------- */
  const map = document.getElementById("map");
  const toggle = document.getElementById("map-toggle");
  if (map && toggle) {
    document.addEventListener("keydown", e => {
      if (e.key === "Escape" && toggle.checked) { toggle.checked = false; toggle.focus(); }
    });
    // Enter opens the menu too, as it would a button.
    toggle.addEventListener("keydown", e => { if (e.key === "Enter") { e.preventDefault(); toggle.click(); } });
    map.addEventListener("click", e => { if (e.target.closest("a")) toggle.checked = false; });
    // The current page in sight in a long map.
    const current = map.querySelector('ul a[aria-current="page"]');
    if (current && current.offsetTop + current.offsetHeight > map.clientHeight) {
      map.scrollTop = current.offsetTop - map.clientHeight / 3;
    }
  }

  // "On this page": the section whose heading was last scrolled past the
  // top third of the window, or the first.
  const toc = [...document.querySelectorAll('.toc a[href^="#"]')];
  const headings = toc.map(a => document.getElementById(decodeURIComponent(a.hash.slice(1))));
  if (toc.length && headings.every(Boolean)) {
    let queued = false;
    const spy = () => {
      queued = false;
      let at = 0;
      headings.forEach((h, i) => { if (h.getBoundingClientRect().top < innerHeight / 3) at = i; });
      toc.forEach((a, i) => (i === at ? a.setAttribute("aria-current", "true") : a.removeAttribute("aria-current")));
    };
    addEventListener("scroll", () => { if (!queued) { queued = true; requestAnimationFrame(spy); } }, { passive: true });
    spy();
  }

  /* ---------- the OpenWrt version ---------- */
  const pick = document.getElementById("openwrt-target");
  if (pick) {
    const show = () => {
      for (const el of document.querySelectorAll(".target")) el.hidden = el.dataset.target !== pick.value;
      for (const el of document.querySelectorAll(".target-label")) el.hidden = true;
    };
    pick.addEventListener("change", show);
    pick.closest(".target-pick").hidden = false;
    show();
  }
})();

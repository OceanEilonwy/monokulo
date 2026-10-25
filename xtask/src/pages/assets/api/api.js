// The API explorer's script (/api/, xtask/src/pages/api_explorer.rs): what
// only a script adds to a page that reads fully without one.
//
// - The switches: each request's language (all of them at once) and each
//   response's status and recorded example.
// - Copy, for a path and a request.
// - Try it: the banner says where requests go and with which key; its
//   Settings and every operation's Try it open the one dialog (the ui's,
//   whose own script opens and closes it), with the reader's address and
//   key, and for an operation its form (the page's <template>) and its
//   live answer, which stays in the operation's Response panel after.
//
// The address and key are kept in sessionStorage only, and sent only to
// that address, never with cookies. Everything an answer says is drawn as
// text (textContent), never as markup.
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const dialog = $("#try-it");
  if (!dialog) return;
  document.documentElement.classList.add("js");

  const store = {
    get(name) { try { return sessionStorage.getItem(`try-it-${name}`) || ""; } catch { return ""; } },
    set(name, value) { try { sessionStorage.setItem(`try-it-${name}`, value); } catch { /* a tab that keeps nothing */ } },
  };
  const server = dialog.dataset.server;
  const KEYS = { sk: "secret", ak: "app", pk: "public" };
  const keyKind = key => (/^(sk|ak|pk)_/.exec(key) || [])[1] || null;

  // A Monokulo's address as typed, read as an absolute http(s) URL with
  // nothing but a host and maybe a path: what every request is built from,
  // so a key only ever goes there. Anything else (no scheme, which a
  // browser would resolve against this site, a query, a fragment, a user
  // name) is refused, as { error }.
  function parseAddress(typed) {
    let url;
    try { url = new URL(typed.trim()); } catch { return { error: "Start the address with https:// (or http://)." }; }
    if (url.protocol !== "https:" && url.protocol !== "http:") return { error: "Start the address with https:// (or http://)." };
    if (url.username || url.password) return { error: "Leave the user name and password out of the address." };
    if (url.search || url.hash || typed.includes("?") || typed.includes("#")) return { error: "Leave the ? and # parts out of the address." };
    return { url, prefix: url.pathname.replace(/\/+$/, "") };
  }
  // The request URL for `path` (and `query`) on the parsed address, built
  // from its origin, never by joining strings.
  function endpoint(address, path, query) {
    const url = new URL(address.url.origin);
    url.pathname = address.prefix + path;
    if (query && query.size) url.search = query.toString();
    return url;
  }
  const kept = parseAddress(store.get("base"));
  const conn = { address: kept.url ? kept : parseAddress(server), key: store.get("key"), checked: null };
  const base = () => conn.address.url.origin + conn.address.prefix;

  // An element, its class, attributes and text or children.
  function el(tag, props = {}, ...children) {
    const node = document.createElement(tag);
    for (const [name, value] of Object.entries(props)) {
      if (name === "class") node.className = value;
      else if (value !== false && value != null) node.setAttribute(name, value === true ? "" : value);
    }
    node.append(...children.filter(c => c != null && c !== false));
    return node;
  }
  const outcome = status => ({ 2: "ok", 3: "ok", 4: "client-error", 5: "server-error" })[String(status)[0]] || "other";
  const statusTag = status => el("span", { class: "mk-http-status", "data-outcome": outcome(status) }, String(status));
  const tag = (text, tone) => el("span", { class: tone ? `tag tag-${tone}` : "tag" }, text);
  const banner = (tone, text) => el("div", { class: "mk-status-banner", "data-tone": tone }, el("div", { class: "mk-status-banner-title" }, el("strong", {}, text)));

  /* ---------- the switches ---------- */
  let lang = "curl";
  try { lang = localStorage.getItem("api-language") || lang; } catch { /* no storage */ }
  function showLanguage(name) {
    lang = name;
    try { localStorage.setItem("api-language", name); } catch { /* no storage */ }
    $$("[data-snippet]").forEach(f => f.toggleAttribute("data-shown", f.dataset.snippet === name));
    $$("[data-lang]").forEach(b => b.setAttribute("aria-pressed", b.dataset.lang === name));
  }
  function showStatus(section, status) {
    $$(".api-example", section).forEach(d => {
      d.open = true;
      d.toggleAttribute("data-shown", d.dataset.status === status);
    });
    $$("[data-status]", section).forEach(b => b.matches("button") && b.setAttribute("aria-pressed", b.dataset.status === status));
  }
  function showExample(details, name) {
    $$(".api-variant", details).forEach(f => f.toggleAttribute("data-shown", f.dataset.example === name));
    $$("button[data-example]", details).forEach(b => b.setAttribute("aria-pressed", b.dataset.example === name));
  }
  showLanguage(lang);
  $$(".api-op").forEach(section => {
    const pressed = $('button[data-status][aria-pressed="true"]', section);
    if (pressed) showStatus(section, pressed.dataset.status);
  });

  async function copy(text, button) {
    const label = button.textContent;
    try { await navigator.clipboard.writeText(text); button.textContent = "Copied"; } catch { button.textContent = "Select it to copy"; }
    setTimeout(() => { button.textContent = label; }, 1400);
  }

  /* ---------- the reader's address and key, everywhere they show ---------- */
  function preferredKey(section) {
    const keys = (section.dataset.keys || "").split(" ").filter(Boolean);
    const mine = keyKind(conn.key);
    return mine && keys.includes(mine) ? mine : ["sk", "ak", "pk"].find(k => keys.includes(k));
  }
  function drawAddress() {
    $$("[data-base]").forEach(s => { s.textContent = base(); });
    $$("[data-host]").forEach(s => { s.textContent = conn.address.url.host; });
    $$("[data-prefix]").forEach(s => { s.textContent = conn.address.prefix; });
    $$(".api-op").forEach(section => {
      const key = preferredKey(section);
      if (key) $$("[data-key]", section).forEach(s => { s.textContent = `${key}_…`; });
    });
    const kind = keyKind(conn.key);
    const title = $("#try-it-banner .mk-status-banner-title strong");
    title.replaceChildren("Try it sends requests to ", el("code", {}, base()), kind ? " with your " : " with no key yet", kind ? el("code", {}, `${kind}_`) : "", kind ? ` ${KEYS[kind]} key` : "");
    const tone = conn.checked === true ? "success" : conn.checked === false ? "error" : "notice";
    $("#try-it-banner").dataset.tone = tone;
  }

  /* ---------- the dialog ---------- */
  const address = $("#try-it-address");
  const keyInput = $("#try-it-key");
  const state = $("#try-it-state");
  const slot = $("#try-it-operation");
  let current = null; // the operation's section, while the dialog is open for it

  function setState(tone, text) {
    state.className = `tag tag-${tone}`;
    state.textContent = text;
  }
  // Reads the address and key as typed; returns whether the address is one
  // to send to. One that isn't keeps the last good one in use, is marked
  // on its field, and isn't kept.
  function readConnection({ report = false } = {}) {
    conn.key = keyInput.value.trim();
    store.set("key", conn.key);
    const typed = address.value.trim();
    const parsed = parseAddress(typed || server);
    address.setCustomValidity(parsed.error || "");
    if (parsed.error) {
      if (report) address.reportValidity();
      return false;
    }
    conn.address = parsed;
    store.set("base", typed);
    return true;
  }
  function connectionChanged() {
    readConnection();
    if (conn.checked !== null) setState("unknown", "Not checked");
    conn.checked = null;
    drawAddress();
    if (current) drawWarnings(current);
  }

  // What the reader should know before sending: a key the route needs or
  // won't take, a public key's challenge.
  function drawWarnings(section) {
    const box = $(".try-it-warnings", slot);
    if (!box) return;
    const keys = (section.dataset.keys || "").split(" ").filter(Boolean);
    const anyone = section.hasAttribute("data-anyone");
    const mine = keyKind(conn.key);
    const warnings = [];
    if (!conn.key && !anyone) warnings.push("This route needs a key: add one above.");
    else if (conn.key && !mine) warnings.push("That doesn't look like a Monokulo key: it starts sk_, ak_ or pk_. Monokulo will answer 401.");
    else if (mine && !anyone && !keys.includes(mine)) warnings.push(`A ${mine}_ key may not use this route: Monokulo will answer 403. It takes ${keys.map(k => `${k}_`).join(", ")}.`);
    if (mine === "pk" && (keys.includes("pk") || anyone)) warnings.push("A public key may be asked to solve a small proof of work first (429 challenge-required), which the JS library does and Try it doesn't. A store that takes requests only from its verified websites refuses this page.");
    box.replaceChildren(...warnings.map(w => banner("warning", w)));
  }

  function open(section) {
    current = section;
    address.value = store.get("base");
    keyInput.value = conn.key;
    const route = $("#try-it-route");
    $$("[data-for]", dialog).forEach(b => { b.hidden = b.dataset.for !== (section ? "operation" : "settings"); });
    if (section) {
      $("#try-it-heading").textContent = $("h3", section).textContent;
      route.textContent = `${section.dataset.method} ${section.dataset.path}`;
      route.hidden = false;
      const form = $(`template[data-try-form="${section.dataset.op}"]`, section).content.cloneNode(true);
      const response = el("div", { id: "try-it-response" });
      slot.replaceChildren(form, response);
      // What was typed for this operation last time.
      const typed = ops.get(section).typed;
      if (typed) $$("input, textarea", slot).forEach(i => { if (i.name in typed) i.value = typed[i.name]; });
      drawWarnings(section);
      drawResponse(section);
    } else {
      $("#try-it-heading").textContent = "Try it with your Monokulo";
      route.textContent = "";
      route.hidden = true;
      slot.replaceChildren();
    }
    // The ui's dialog focuses its first field; the key, while there is
    // none, else the operation's form.
    setTimeout(() => (conn.key ? $("input, textarea", slot) || keyInput : keyInput).focus());
  }

  async function checkConnection() {
    if (!readConnection({ report: true })) {
      setState("error", "Not an address");
      return;
    }
    setState("syncing", "Checking…");
    try {
      const headers = { Accept: "application/prs.hal-forms+json" };
      if (conn.key) headers.Authorization = `Bearer ${conn.key}`;
      const res = await fetch(endpoint(conn.address, "/api/v1"), { headers, credentials: "omit", cache: "no-store" });
      const body = await res.json().catch(() => null);
      conn.checked = res.ok;
      if (res.ok) {
        const forms = body && body._templates ? Object.keys(body._templates).length : 0;
        setState("ok", `Found it: your key may use ${forms} form${forms === 1 ? "" : "s"}`);
      } else {
        setState("error", `${res.status} ${(body && typeof body.title === "string" && body.title) || ""}`.trim());
      }
    } catch {
      conn.checked = false;
      setState("error", "Can't reach it");
    }
    drawAddress();
  }

  /* ---------- sending, and the live answer ---------- */
  // Each operation's live answer and what was typed for it.
  const ops = new Map($$(".api-op").map(section => [section, { live: null, abort: null, typed: null }]));

  const PROBLEM = /^https:\/\/docs\.monokulo\.com\/problems\/([a-z0-9-]+)$/;

  // The answer, drawn as text into `box`.
  function drawLive(section, box) {
    const live = ops.get(section).live;
    const bar = el("div", { class: "api-panel-bar" });
    if (live.error) {
      bar.append(tag("No answer", "error"), el("button", { class: "mk-btn", type: "button", "data-size": "compact", "data-clear": true }, "Show recorded examples"));
      // The recorded example shown for its first status (a success).
      const recorded = $(".api-example .api-variant[data-shown] pre", section);
      box.replaceChildren(bar, banner("error", live.error), recorded ? el("p", { class: "api-note" }, tag("Recorded", "muted"), " What an answer looks like:") : "", recorded ? recorded.cloneNode(true) : "");
      return;
    }
    bar.append(tag(live.pending ? "Live, arriving…" : "Live", live.pending ? "syncing" : "ok"));
    if (live.status != null) bar.append(statusTag(live.status), `${live.ms} ms`);
    // While it's on its way: Stop for a stream, nothing for the rest.
    if (live.pending && live.stream) bar.append(el("button", { class: "mk-btn", type: "button", "data-size": "compact", "data-stop": true }, "Stop"));
    else if (!live.pending) bar.append(el("button", { class: "mk-btn", type: "button", "data-size": "compact", "data-clear": true }, "Show recorded examples"));
    const parts = [bar];
    if (live.headers) {
      parts.push(el("details", {}, el("summary", {}, "Headers a browser may read"), el("pre", {}, el("code", {}, live.headers.map(([k, v]) => `${k}: ${v}`).join("\n")))));
    }
    let text = live.text || "";
    try { if (!live.stream && text) text = JSON.stringify(JSON.parse(text), null, 2); } catch { /* not JSON: as it came */ }
    parts.push(el("pre", {}, el("code", {}, text || (live.pending ? "" : "(no body)"))));
    const problem = live.problem && PROBLEM.exec(live.problem);
    if (problem) {
      parts.push(el("p", { class: "api-note" }, "This problem's page: ", el("a", { href: `../problems/${problem[1]}/` }, problem[1])));
    }
    box.replaceChildren(...parts);
  }

  function drawResponse(section) {
    const live = ops.get(section).live;
    const panel = $(".api-live", section);
    $(".api-recorded", section).hidden = !!live;
    $("[data-recorded]", section).hidden = !!live;
    panel.hidden = !live;
    if (live) drawLive(section, panel);
    else panel.replaceChildren();
    const inDialog = current === section && $("#try-it-response");
    if (inDialog) {
      const box = live ? el("div", { class: "api-live" }) : null;
      if (box) drawLive(section, box);
      inDialog.replaceChildren(el("h3", {}, "Response"), box || el("p", { class: "hint" }, "Send the request to see your Monokulo's answer here. It stays beside the operation on the page too."));
    }
  }

  async function send(section, form) {
    const op = ops.get(section);
    op.typed = Object.fromEntries($$("input, textarea", form).map(i => [i.name, i.value]));
    if (!readConnection({ report: true })) return;
    let path = section.dataset.path;
    for (const input of $$("[data-path-param]", form)) path = path.replace(`{${input.dataset.pathParam}}`, encodeURIComponent(input.value.trim()));
    const query = new URLSearchParams($$("[data-query-param]", form).filter(i => i.value.trim()).map(i => [i.dataset.queryParam, i.value.trim()]));
    const body = $("[data-body]", form);
    if (body) {
      try { JSON.parse(body.value); } catch (e) {
        body.setCustomValidity(`This isn't JSON: ${e.message}`);
        body.reportValidity();
        body.addEventListener("input", () => body.setCustomValidity(""), { once: true });
        return;
      }
    }
    const stream = section.hasAttribute("data-stream");
    const headers = { Accept: stream ? "text/event-stream" : "application/prs.hal-forms+json, application/json, application/problem+json" };
    if (conn.key) headers.Authorization = `Bearer ${conn.key}`;
    if (body) headers["Content-Type"] = "application/json";
    op.abort?.abort();
    const abort = op.abort = new AbortController();
    op.live = { pending: true, stream, text: "" };
    drawResponse(section);
    const started = performance.now();
    const target = endpoint(conn.address, path, query);
    try {
      const res = await fetch(target, { method: section.dataset.method, headers, body: body ? body.value : undefined, signal: abort.signal, credentials: "omit", cache: "no-store" });
      op.live = { pending: true, stream, text: "", status: res.status, headers: [...res.headers], ms: Math.round(performance.now() - started) };
      if (stream && res.ok && res.body) {
        drawResponse(section);
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          op.live.text += value;
          $$(`.api-live pre code`, section).concat(current === section ? $$("#try-it-response .api-live > pre code") : []).forEach(code => { code.textContent = op.live.text; });
        }
      } else {
        op.live.text = await res.text();
        try { const problem = JSON.parse(op.live.text).type; if (typeof problem === "string") op.live.problem = problem; } catch { /* not a problem */ }
      }
      op.live.pending = false;
    } catch (error) {
      if (error.name === "AbortError" && op.live?.status != null) {
        op.live.pending = false; // stopped: what arrived stays
      } else if (error.name !== "AbortError") {
        const kind = keyKind(conn.key);
        op.live = { error: `Couldn't reach ${base()}: it may be offline, or the address wrong, or it refused a request from this page${kind === "pk" ? " (a store that takes requests only from its verified websites refuses it)" : ""}. Your browser said: ${error.message}` };
      }
    }
    if (op.abort === abort) op.abort = null;
    drawResponse(section);
  }

  /* ---------- events ---------- */
  // Before the ui's dialog script opens the dialog: fill it for the
  // operation, or the settings.
  addEventListener("click", event => {
    const opener = event.target.closest?.("[data-try]");
    if (opener) open(opener.dataset.try ? document.getElementById(opener.dataset.try) : null);
  }, true);

  document.addEventListener("click", event => {
    const t = event.target;
    const section = t.closest(".api-op");
    const button = t.closest("button");
    if (!button) return;
    if (button.matches("[data-lang]")) showLanguage(button.dataset.lang);
    else if (button.matches("[data-status]") && section) showStatus(section, button.dataset.status);
    else if (button.matches("[data-example]")) showExample(button.closest(".api-example"), button.dataset.example);
    else if (button.matches("[data-copy-path]")) copy(base() + button.dataset.copyPath, button);
    else if (button.matches("[data-copy-snippet]") && section) copy($("[data-snippet][data-shown] pre", section).textContent, button);
    else if (button.matches("#try-it-check")) checkConnection();
    else if (button.matches("#try-it-show")) {
      // "Show the key", pressed while it shows.
      const shown = keyInput.type === "password";
      keyInput.type = shown ? "text" : "password";
      button.setAttribute("aria-pressed", shown);
    } else {
      const target = section || current;
      if (!target) return;
      const op = ops.get(target);
      if (button.matches("[data-stop]")) op.abort?.abort();
      else if (button.matches("[data-clear]")) {
        op.abort?.abort();
        op.live = null;
        drawResponse(target);
      }
    }
  });

  address.addEventListener("input", () => address.setCustomValidity(""));
  address.addEventListener("change", connectionChanged);
  keyInput.addEventListener("change", connectionChanged);
  dialog.addEventListener("submit", event => {
    if (!current || event.target.id !== "try-it-form") return;
    event.preventDefault();
    send(current, event.target);
  });
  dialog.addEventListener("close", () => {
    if (current) {
      const form = $("#try-it-form", slot);
      if (form) ops.get(current).typed = Object.fromEntries($$("input, textarea", form).map(i => [i.name, i.value]));
    }
    readConnection();
    drawAddress();
    current = null;
  });

  drawAddress();
})();

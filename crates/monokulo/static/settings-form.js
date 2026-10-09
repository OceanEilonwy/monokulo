// The settings components (crates/monokulo/src/views/settings.rs): every
// page's settings are cards in one form, with one save bar along the bottom
// of the window. The server renders all of it, so a page works without
// this: the form posts, the bar is always there. This adds:
//
//   <mk-settings-form>  around the page's <form>: tracks what changed, marks
//                       it, asks before leaving with changes, and saves with
//                       fixi (a form with fx-action: its panel is swapped) or
//                       with a plain POST (the page reloads). Sends
//                         mk-dirty  {count, groups}  the changes went up or down
//                         mk-saved  {groups}         the page answers a save
//                         mk-failed {group, message} a save refused a card
//                                                    (group null: no card)
//   <mk-settings-card>  a card: "N unsaved" and Discard while it has changes;
//                       sends mk-count {changes} (a page script adds changes
//                       it tracks itself) and mk-discard (cancelable: the
//                       page script put the card back itself, by reloading).
//                       data-shown-by="name=value": shown only while that
//                       checkbox is ticked, its controls off while hidden.
//   <mk-setting>        one setting: "changed" while it differs from what's
//                       saved (data-saved on a control after a refused save,
//                       its default value otherwise).
//   <mk-save-bar>       the page's Save: hidden until something changes (or
//                       a save was refused), naming the cards changed.
//
// Plus, anywhere on the page: a toast's close button, the Show links to a
// card, and links inside [data-leave-asks] (the admin page's tabs, which
// fixi swaps in without leaving the page) asking before they go while there
// are changes. Other ways off the page get the browser's own question.
//
// One settings form per page. Its state lives here, not on the element:
// fixi swaps the form out for a new one after a save.
(function () {
  "use strict";

  var barAtLoad = null;
  var leavingTo = null;
  var goAfterSave = null;
  var submitting = false;
  // The page is being loaded again (Discard on a card a save refused): no
  // "leave the page?" question.
  var reloading = false;
  // Something was changed since the form came: until then, a save's
  // refusal is what the bar says.
  var touched = false;
  var lastCount = -1;

  function each(root, selector, fn) { Array.prototype.forEach.call(root.querySelectorAll(selector), fn); }
  function settingsForm() { return document.querySelector("mk-settings-form"); }
  function form() { var host = settingsForm(); return host && host.querySelector("form"); }
  function bar() { return document.querySelector("mk-save-bar"); }
  function fire(target, name, detail) { target.dispatchEvent(new CustomEvent(name, { bubbles: true, detail: detail })); }

  // ---- values: what a control holds, and what's saved -------------------

  // A checkbox or a radio button holds whether it's ticked.
  function ticks(el) { return el.type === "checkbox" || el.type === "radio"; }

  function savedValue(el) {
    if (el.hasAttribute("data-saved")) {
      var saved = el.getAttribute("data-saved");
      return ticks(el) ? saved === "on" : saved;
    }
    if (ticks(el)) return el.defaultChecked;
    if (el.tagName === "SELECT") {
      for (var i = 0; i < el.options.length; i++) if (el.options[i].defaultSelected) return el.options[i].value;
      return el.options.length ? el.options[0].value : "";
    }
    return el.defaultValue;
  }

  function currentValue(el) { return ticks(el) ? el.checked : el.value; }

  function restore(el) {
    var saved = savedValue(el);
    if (ticks(el)) el.checked = saved;
    else el.value = saved;
  }

  // What a setting is sent under: its own controls, not hidden ones.
  function controls(setting) {
    return setting.querySelectorAll("input[name]:not([type=hidden]), select[name], textarea[name]");
  }

  function settingChanged(setting) {
    return Array.prototype.some.call(controls(setting), function (el) { return currentValue(el) !== savedValue(el); });
  }

  // ---- cards ------------------------------------------------------------

  function cardTitle(card) {
    var heading = card.querySelector(".card-head h3");
    return heading ? heading.textContent : "";
  }

  function cardChanges(card) {
    var count = 0;
    each(card, "mk-setting", function (setting) {
      var changed = settingChanged(setting);
      setting.classList.toggle("is-changed", changed);
      if (changed) count++;
    });
    var extra = { changes: 0 };
    card.dispatchEvent(new CustomEvent("mk-count", { detail: extra }));
    return count + extra.changes;
  }

  function clearFailure(card) {
    if (!card.classList.contains("is-failed")) return;
    card.classList.remove("is-failed");
    each(card, ".card-body > p.error[role=alert], [data-card-state] .badge-error", function (el) { el.remove(); });
  }

  function discardCard(card) {
    var asked = new CustomEvent("mk-discard", { cancelable: true });
    if (!card.dispatchEvent(asked)) {
      // The page puts it back by loading again.
      reloading = true;
      return;
    }
    each(card, "mk-setting", function (setting) { Array.prototype.forEach.call(controls(setting), restore); });
    clearFailure(card);
    showCards();
  }

  // A card shown only while a checkbox is ticked (data-shown-by): hidden,
  // its controls are disabled too, so what's in them isn't sent unseen.
  function showCards() {
    each(document, "mk-settings-card[data-shown-by]", function (card) {
      var by = card.getAttribute("data-shown-by");
      var at = by.indexOf("=");
      var name = by.slice(0, at), value = by.slice(at + 1);
      var box = Array.prototype.find.call(document.querySelectorAll("input[type=checkbox]"), function (el) {
        return el.name === name && el.value === value;
      });
      if (!box) return;
      card.hidden = !box.checked;
      each(card, "input, select, textarea", function (el) {
        if (card.hidden && !el.disabled) {
          el.disabled = true;
          el.setAttribute("data-hidden-off", "");
        } else if (!card.hidden && el.hasAttribute("data-hidden-off")) {
          el.disabled = false;
          el.removeAttribute("data-hidden-off");
        }
      });
    });
  }

  // ---- the count, the cards' marks and the bar ---------------------------

  function update() {
    var panel = form();
    if (!panel) return 0;
    var total = 0, changed = [];
    each(panel, "mk-settings-card", function (card) {
      var count = card.hidden ? 0 : cardChanges(card);
      // A refused card put back as it's saved has nothing left to refuse.
      if (count === 0 && touched && card.classList.contains("is-failed")) clearFailure(card);
      card.classList.toggle("is-dirty", count > 0);
      var state = card.querySelector("[data-card-state]");
      var badge = state && state.querySelector("[data-unsaved]");
      if (count > 0) {
        if (!badge && state) {
          badge = document.createElement("span");
          badge.className = "badge badge-unsaved";
          badge.setAttribute("data-unsaved", "");
          state.insertBefore(badge, state.firstChild);
        }
        if (badge) badge.textContent = count + " unsaved";
        total += count;
        changed.push(card);
      } else if (badge) {
        badge.remove();
      }
      var discard = card.querySelector("[data-card-discard]");
      if (discard) discard.hidden = count === 0 && !card.classList.contains("is-failed");
      var saved = card.querySelector("[data-card-saved]");
      if (saved) saved.hidden = count > 0;
    });
    showBar(total, changed);
    if (total !== lastCount) {
      lastCount = total;
      fire(settingsForm(), "mk-dirty", { count: total, groups: changed.map(function (card) { return card.getAttribute("name"); }) });
    }
    return total;
  }

  function showBar(total, changed) {
    var box = bar();
    if (!box || leavingTo) return;
    var message = box.querySelector("[data-save-bar-message]");
    box.classList.toggle("is-dirty", total > 0);
    if (total > 0 && (touched || !barAtLoad || !barAtLoad.failed)) {
      box.classList.remove("is-failed");
      message.textContent = "";
      var strong = document.createElement("strong");
      strong.textContent = total + (total === 1 ? " unsaved change" : " unsaved changes");
      message.appendChild(strong);
      message.appendChild(document.createTextNode(" in "));
      changed.forEach(function (card, i) {
        if (i > 0) message.appendChild(document.createTextNode(i === changed.length - 1 ? " and " : ", "));
        var link = document.createElement("a");
        link.href = "#" + card.id;
        link.setAttribute("data-show-card", card.getAttribute("name"));
        link.textContent = cardTitle(card);
        message.appendChild(link);
      });
    } else if (barAtLoad && (total === 0 || barAtLoad.failed)) {
      message.innerHTML = barAtLoad.html;
      // A refusal on cards lasts while one of them still shows it; one
      // that names no card, until the next save.
      var stillFailed = barAtLoad.cards === 0 || document.querySelector("mk-settings-card.is-failed") !== null;
      box.classList.toggle("is-failed", barAtLoad.failed && stillFailed);
    }
  }

  function discardAll() {
    each(document, "mk-settings-card.is-dirty, mk-settings-card.is-failed", discardCard);
    update();
  }

  function changedNow() {
    touched = true;
    update();
  }

  // ---- leaving with unsaved changes ------------------------------------

  function leaveAsk(link) {
    var box = bar();
    if (!box) return;
    leavingTo = link.getAttribute("href");
    box.classList.add("is-leaving");
    var message = box.querySelector("[data-save-bar-message]");
    var host = settingsForm();
    message.textContent = "";
    var strong = document.createElement("strong");
    strong.textContent = ((host && host.getAttribute("label")) || "This page") + " has unsaved changes.";
    message.appendChild(strong);
    message.appendChild(document.createTextNode(" Save or discard them before going to " + link.textContent.replace(/\(needs attention\)/, "").trim() + "."));
    var actions = box.querySelector(".save-bar-actions");
    actions.hidden = true;
    var ask = document.createElement("div");
    ask.className = "save-bar-actions";
    ask.setAttribute("data-leave-actions", "");
    [["Stay here", "stay", ""], ["Discard and go", "discard", ""], ["Save and go", "save", "btn-primary"]].forEach(function (choice) {
      var button = document.createElement("button");
      button.type = "button";
      button.textContent = choice[0];
      button.setAttribute("data-leave", choice[1]);
      if (choice[2]) button.className = choice[2];
      ask.appendChild(button);
    });
    (box.querySelector(".save-bar-inner") || box).appendChild(ask);
    ask.lastChild.focus();
  }

  function leaveDone() {
    var box = bar();
    leavingTo = null;
    if (!box) return;
    box.classList.remove("is-leaving");
    each(box, "[data-leave-actions]", function (el) { el.remove(); });
    var actions = box.querySelector(".save-bar-actions");
    if (actions) actions.hidden = false;
    update();
  }

  function go(href) {
    var link = document.querySelector('[data-leave-asks] a[href="' + href + '"]');
    // Marked only for this click (the capture handler below runs inside
    // it), so a later click on the same link still asks.
    if (link) {
      link.__go = true;
      link.click();
      link.__go = false;
    } else {
      location.href = href;
    }
  }

  function showCard(name) {
    var card = document.getElementById("card-" + name);
    if (!card) return false;
    var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    card.scrollIntoView({ block: "start", behavior: reduce ? "auto" : "smooth" });
    var problem = card.querySelector('[aria-invalid="true"], .setting-problem');
    var row = problem && problem.closest("details");
    if (row) row.open = true;
    var control = (problem && problem.closest(".setting-field") || card).querySelector("input:not([disabled]):not([type=hidden]), select, textarea");
    if (control) control.focus({ preventScroll: true });
    return true;
  }

  // ---- clicks -----------------------------------------------------------

  // Before fixi (capture): a link that asks first, with unsaved changes.
  document.addEventListener("click", function (event) {
    var link = event.target.closest && event.target.closest("[data-leave-asks] a");
    if (!link || link.__go || link.getAttribute("aria-current") === "page" || !form()) return;
    if (update() > 0) {
      event.preventDefault();
      event.stopImmediatePropagation();
      leaveAsk(link);
    }
  }, true);

  document.addEventListener("click", function (event) {
    var target = event.target.closest ? event.target : null;
    if (!target) return;
    var discard = target.closest("[data-card-discard]");
    if (discard) { touched = true; discardCard(discard.closest("mk-settings-card")); update(); return; }
    if (target.closest("[data-discard-all]") && form()) { event.preventDefault(); discardAll(); return; }
    var leave = target.closest("[data-leave]");
    if (leave) {
      var href = leavingTo;
      var choice = leave.getAttribute("data-leave");
      leaveDone();
      if (choice === "discard") {
        discardAll();
        if (!reloading) go(href);
      }
      if (choice === "save") {
        goAfterSave = href;
        var save = bar() && bar().querySelector("[data-save]");
        form().requestSubmit(save || undefined);
      }
      return;
    }
    var close = target.closest("[data-toast-close]");
    if (close) {
      var toast = close.closest("[data-toast]");
      toast.classList.add("is-closing");
      setTimeout(function () { toast.remove(); }, 260);
      return;
    }
    var show = target.closest("[data-show-card]");
    if (show && showCard(show.getAttribute("data-show-card"))) event.preventDefault();
  });

  // ---- typing and ticking ----------------------------------------------

  function changedHere(event) {
    var panel = form();
    if (!panel || !event.target.closest || !panel.contains(event.target)) return;
    // A control another form owns (form="…") isn't a setting.
    if (event.target.form && event.target.form !== panel) return;
    if (event.target.type === "checkbox") showCards();
    changedNow();
  }
  document.addEventListener("input", changedHere);
  document.addEventListener("change", changedHere);

  // ---- saving -----------------------------------------------------------

  // Before fixi (capture), but after a page's own scripts' capture
  // listeners, which load first: a save one of them held back (a
  // confirm(), a question first) isn't counted as sent.
  document.addEventListener("submit", function (event) {
    if (event.target !== form()) return;
    if (event.defaultPrevented) {
      goAfterSave = null;
      return;
    }
    submitting = true;
  }, true);

  window.addEventListener("beforeunload", function (event) {
    if (!submitting && !reloading && form() && update() > 0) {
      event.preventDefault();
      event.returnValue = "";
    }
  });

  // A save that comes back without a panel to swap in (a dropped
  // connection, or any failure but 422, which fixi's glue doesn't swap):
  // the page guards its unsaved changes again, and doesn't go anywhere.
  // fixi says "finally" before it swaps, so a save that will swap is left to
  // `settle`.
  document.addEventListener("fx:finally", function (event) {
    var panel = form();
    if (!panel || event.target !== panel) return;
    var response = event.detail && event.detail.cfg && event.detail.cfg.response;
    if (response && (response.ok || response.status === 422)) return;
    submitting = false;
    goAfterSave = null;
  });

  // ---- a form arriving: the page loading, or fixi swapping one in ------

  function settle() {
    var host = settingsForm();
    if (!host) return;
    submitting = false;
    touched = false;
    leavingTo = null;
    lastCount = -1;
    var box = bar();
    var message = box && box.querySelector("[data-save-bar-message]");
    barAtLoad = message
      ? { html: message.innerHTML, failed: box.classList.contains("is-failed"), cards: document.querySelectorAll("mk-settings-card.is-failed").length }
      : null;
    showCards();
    update();
    // What the save this page answers did.
    var failed = document.querySelectorAll("mk-settings-card.is-failed");
    Array.prototype.forEach.call(failed, function (card) {
      var why = card.querySelector(".card-body > p.error[role=alert]");
      fire(host, "mk-failed", { group: card.getAttribute("name"), message: why ? why.textContent : "" });
    });
    if (!failed.length && box && box.classList.contains("is-failed")) {
      fire(host, "mk-failed", { group: null, message: message.textContent });
    }
    var saved = Array.prototype.map.call(document.querySelectorAll("mk-settings-card [data-card-saved]"), function (mark) {
      return mark.closest("mk-settings-card").getAttribute("name");
    });
    if (saved.length) fire(host, "mk-saved", { groups: saved });
    if (goAfterSave) {
      var href = goAfterSave;
      goAfterSave = null;
      if (!failed.length && !(box && box.classList.contains("is-failed"))) go(href);
    }
  }

  // Elements: each upgrades once the page is parsed (this script is
  // deferred) or as fixi swaps it in. A form settles a task later, once
  // its cards are in and fixi's glue has put the rest of a swap's answer in
  // place (the tab bar it brings, the focus).
  function define(name, element) {
    if (window.customElements && !customElements.get(name)) customElements.define(name, element);
  }

  if (!window.customElements) return;

  define("mk-setting", class extends HTMLElement {
    get changed() { return settingChanged(this); }
  });
  define("mk-settings-card", class extends HTMLElement {
    get changes() { return this.hidden ? 0 : cardChanges(this); }
    discard() { touched = true; discardCard(this); update(); }
  });
  define("mk-save-bar", class extends HTMLElement {});
  define("mk-settings-form", class extends HTMLElement {
    connectedCallback() {
      var self = this;
      setTimeout(function () { if (self.isConnected) settle(); }, 0);
    }
    get changes() { return update(); }
    discardAll() { discardAll(); }
    // A page script changed something a setting doesn't hold (a node row
    // moved).
    changed() { changedNow(); }
  });
})();

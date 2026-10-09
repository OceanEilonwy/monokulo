// The admin settings page with JavaScript (crates/monokulo/src/views/admin.rs),
// and the account page's cards (views/account.rs), which use the same markup.
// Without it the page already works: one form per tab, a save bar that is
// always there, and buttons that save at once. This adds:
//   - unsaved changes: each card with a change is marked, with its own
//     Discard, and the save bar appears, naming the changed cards;
//   - leaving a tab (or the page) with unsaved changes asks first;
//   - node rows reorder by dragging their handle (or its arrow keys), and
//     Remove takes a row out; both are unsaved changes like any other, and
//     a new order counts as one change however many rows it moves;
//   - toasts that say something wasn't saved stay until closed;
//   - a row's self-signed box shows only while its Use TLS box is ticked,
//     and a key custody backend's card only while the backend is ticked;
//   - the confirmation before a save leaves a network stores use with no
//     node.
// Everything listens on the document, so it keeps working on the panel
// fixi swaps in after a save or a tab link.
(function () {
  "use strict";

  var listsAtLoad = {};
  var barAtLoad = null;
  var leavingTo = null;
  var goAfterSave = null;
  var submitting = false;
  // The page is being loaded again (Discard on a card a save refused): no
  // "leave the page?" question, and no other navigation after it.
  var reloading = false;
  // Something was changed since the panel came: until then, a save's
  // refusal is what the bar says.
  var touched = false;

  function form() { return document.getElementById("settings-form"); }
  function bar() { return document.getElementById("save-bar"); }
  function each(root, selector, fn) { Array.prototype.forEach.call(root.querySelectorAll(selector), fn); }

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

  // The controls a card's own settings are sent under (not node rows').
  function settingControls(field) {
    return field.querySelectorAll("input[name]:not([type=hidden]), select[name], textarea[name]");
  }

  function fieldChanged(field) {
    return Array.prototype.some.call(settingControls(field), function (el) { return currentValue(el) !== savedValue(el); });
  }

  // ---- node rows --------------------------------------------------------

  function rowControls(row) { return row.querySelectorAll("input[name]"); }

  function rowEdited(row) {
    return Array.prototype.some.call(rowControls(row), function (el) { return currentValue(el) !== savedValue(el); });
  }

  function savedIndex(row) {
    var at = row.getAttribute("data-node-saved");
    return at === null ? null : Number(at);
  }

  // A network's changes: rows edited, added or taken out, and one more
  // for a new order of the rows that were there.
  function networkChanges(card) {
    var rows = Array.prototype.slice.call(card.querySelectorAll("[data-node-row]"));
    var savedCount = Number(card.getAttribute("data-saved-count")) || 0;
    var order = rows.map(savedIndex).filter(function (at) { return at !== null; });
    var moved = order.some(function (at, i) { return i > 0 && at < order[i - 1]; });
    var count = Math.max(0, savedCount - order.length) + (moved ? 1 : 0);
    rows.forEach(function (row) {
      var text = "";
      if (row.hasAttribute("data-node-add")) {
        var address = row.querySelector('input[name$="_address"]');
        if (address && address.value.trim() !== "") text = "new";
      } else if (savedIndex(row) === null || rowEdited(row)) {
        text = "changed";
      } else if (moved && row.hasAttribute("data-node-moved")) {
        text = "moved";
      }
      if (text === "new" || text === "changed") count++;
      if (!moved) row.removeAttribute("data-node-moved");
      row.classList.toggle("is-changed", text !== "");
      var mark = row.querySelector("[data-node-mark]");
      if (mark) mark.innerHTML = text ? '<span class="badge badge-unsaved">' + text + "</span>" : "";
    });
    return count;
  }

  // After rows move, are added or taken out: each row's fields are named
  // for its place again (the form is read in that order), and its place
  // is said again (Primary, Fallback N).
  function renumber(list) {
    var network = list.getAttribute("data-node-rows");
    var rows = list.querySelectorAll("[data-node-row]");
    // Global: aria-describedby can name several ids (the help and an error).
    var names = new RegExp("node_" + network + "_\\d+_", "g"), ids = new RegExp("node-" + network + "-\\d+-", "g");
    var place = 0;
    Array.prototype.forEach.call(rows, function (row, i) {
      row.setAttribute("data-node-row", String(i));
      each(row, "[name],[id],[for],[aria-describedby]", function (el) {
        ["name", "id", "for", "aria-describedby"].forEach(function (attr) {
          if (!el.hasAttribute(attr)) return;
          el.setAttribute(attr, el.getAttribute(attr).replace(names, "node_" + network + "_" + i + "_").replace(ids, "node-" + network + "-" + i + "-"));
        });
      });
      each(row, 'button[name="node_action"]', function (button) {
        button.value = button.value.replace(/:\d+$/, ":" + i);
      });
      if (!row.hasAttribute("data-node-add")) {
        var label = place === 0 ? "Primary" : "Fallback " + place;
        var shown = row.querySelector("[data-node-place]");
        if (shown) shown.textContent = label;
        var handle = row.querySelector("[data-node-handle]");
        if (handle) handle.setAttribute("aria-label", "Reorder " + label + ": press the up or down arrow");
        place++;
      }
    });
  }

  function showSelfSigned(root) {
    each(root, "[data-node-tls]", function (box) {
      var row = box.closest("[data-node-row]");
      var field = row && row.querySelector("[data-node-self-signed]");
      if (field) field.hidden = !box.checked;
    });
  }

  // A backend's card shows only while it's ticked; hidden, its controls
  // are disabled too, so what's in them isn't sent (or saved) unseen.
  function showCustodyBackends(root) {
    each(root, "input[type=checkbox]", function (box) {
      if (box.name !== "key_custody.enabled_backends") return;
      var card = document.querySelector('[data-custody-backend="' + box.value + '"]');
      if (!card) return;
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

  // ---- cards and the save bar ------------------------------------------

  function cardTitle(card) {
    var heading = card.querySelector(".card-head h3");
    return heading ? heading.textContent : "";
  }

  function cardChanges(card) {
    var count = 0;
    each(card, ".setting-field", function (field) {
      if (field.closest("[data-node-row]")) return;
      var changed = fieldChanged(field);
      field.classList.toggle("is-changed", changed);
      if (changed) count++;
    });
    if (card.hasAttribute("data-network")) count += networkChanges(card);
    return count;
  }

  function update() {
    var panel = form();
    if (!panel) return 0;
    var total = 0, changed = [];
    each(panel, "[data-card]", function (card) {
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
        link.setAttribute("data-show-card", card.getAttribute("data-card"));
        link.textContent = cardTitle(card);
        message.appendChild(link);
      });
    } else if (barAtLoad && (total === 0 || barAtLoad.failed)) {
      message.innerHTML = barAtLoad.html;
      // A refusal on cards lasts while one of them still shows it; one
      // that names no card, until the next save.
      var stillFailed = barAtLoad.cards === 0 || document.querySelector("[data-card].is-failed") !== null;
      box.classList.toggle("is-failed", barAtLoad.failed && stillFailed);
    }
  }

  function discardCard(card) {
    if (card.hasAttribute("data-network")) {
      var list = card.querySelector("[data-node-rows]");
      // A network a save refused shows the rows as sent: its saved rows
      // are only on the server.
      if (card.classList.contains("is-failed") || !listsAtLoad[card.id]) {
        var tab = document.getElementById("settings-panel");
        reloading = true;
        location.href = "/dashboard/admin/settings?tab=" + (tab ? tab.getAttribute("data-tab") : "nodes");
        return;
      }
      var fresh = listsAtLoad[card.id].cloneNode(true);
      list.replaceWith(fresh);
      showSelfSigned(card);
    }
    each(card, ".setting-field", function (field) {
      if (field.closest("[data-node-row]")) return;
      Array.prototype.forEach.call(settingControls(field), restore);
    });
    clearFailure(card);
    showCustodyBackends(document);
  }

  function clearFailure(card) {
    if (!card.classList.contains("is-failed")) return;
    card.classList.remove("is-failed");
    each(card, ".card-body > p.error[role=alert], [data-card-state] .badge-error", function (el) { el.remove(); });
  }

  function discardAll() {
    each(document, "[data-card].is-dirty, [data-card].is-failed", discardCard);
    update();
  }

  // ---- leaving with unsaved changes ------------------------------------

  function leaveAsk(link) {
    var box = bar();
    if (!box) return;
    leavingTo = link.getAttribute("href");
    box.classList.add("is-leaving");
    var message = box.querySelector("[data-save-bar-message]");
    var here = document.getElementById("settings-panel");
    message.textContent = "";
    var strong = document.createElement("strong");
    strong.textContent = (here ? here.getAttribute("data-tab-label") : "This tab") + " has unsaved changes.";
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
    var link = document.querySelector('#settings-tabs a[href="' + href + '"]');
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

  // ---- reordering node rows --------------------------------------------

  var dragging = null;

  function moveRow(row, before) {
    var list = row.parentNode;
    touched = true;
    list.insertBefore(row, before);
    row.setAttribute("data-node-moved", "");
    renumber(list);
    update();
  }

  function clearDrop() {
    each(document, ".is-drop-before, .is-drop-after", function (el) { el.classList.remove("is-drop-before", "is-drop-after"); });
  }

  // Pointer events, so a finger drags a row as a mouse does.
  document.addEventListener("pointerdown", function (event) {
    var handle = event.target.closest && event.target.closest("[data-node-handle]");
    if (!handle || event.button !== 0) return;
    event.preventDefault();
    dragging = handle.closest("[data-node-row]");
    dragging.classList.add("is-dragging");
    handle.setPointerCapture(event.pointerId);
  });

  document.addEventListener("pointermove", function (event) {
    if (!dragging) return;
    clearDrop();
    var under = document.elementFromPoint(event.clientX, event.clientY);
    var row = under && under.closest("[data-node-row]");
    if (!row || row === dragging || row.parentNode !== dragging.parentNode) return;
    var box = row.getBoundingClientRect();
    var after = !row.hasAttribute("data-node-add") && event.clientY > box.top + box.height / 2;
    row.classList.add(after ? "is-drop-after" : "is-drop-before");
  });

  function endDrag() {
    if (!dragging) return;
    var target = document.querySelector(".is-drop-before, .is-drop-after");
    var row = dragging;
    dragging = null;
    row.classList.remove("is-dragging");
    if (target) moveRow(row, target.classList.contains("is-drop-after") ? target.nextElementSibling : target);
    clearDrop();
  }
  document.addEventListener("pointerup", endDrag);
  document.addEventListener("pointercancel", function () {
    if (dragging) dragging.classList.remove("is-dragging");
    dragging = null;
    clearDrop();
  });

  document.addEventListener("keydown", function (event) {
    var handle = event.target.closest && event.target.closest("[data-node-handle]");
    if (!handle || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
    event.preventDefault();
    var row = handle.closest("[data-node-row]");
    if (event.key === "ArrowUp") {
      var above = row.previousElementSibling;
      if (above) moveRow(row, above);
    } else {
      var below = row.nextElementSibling;
      if (below && !below.hasAttribute("data-node-add")) moveRow(row, below.nextElementSibling);
    }
    var again = row.querySelector("[data-node-handle]");
    if (again) again.focus();
  });

  // ---- clicks -----------------------------------------------------------

  // Before fixi (capture): a tab link with unsaved changes asks first.
  document.addEventListener("click", function (event) {
    var link = event.target.closest && event.target.closest("#settings-tabs a");
    if (!link || link.__go || link.getAttribute("aria-current") === "page") return;
    if (update() > 0) {
      event.preventDefault();
      event.stopImmediatePropagation();
      leaveAsk(link);
    }
  }, true);

  document.addEventListener("click", function (event) {
    var target = event.target.closest ? event.target : null;
    if (!target) return;
    // The handle drags; it doesn't open its row.
    if (target.closest("[data-node-handle]")) { event.preventDefault(); return; }
    var remove = target.closest("[data-node-remove]");
    if (remove) {
      event.preventDefault();
      var row = remove.closest("[data-node-row]");
      var list = row.parentNode;
      touched = true;
      row.remove();
      renumber(list);
      update();
      return;
    }
    var add = target.closest("[data-node-add-another]");
    if (add) {
      var network = add.getAttribute("data-node-add-another");
      var rows = document.querySelector('[data-node-rows="' + network + '"]');
      var blank = rows && rows.querySelector("[data-node-add]:last-of-type");
      if (!blank) return;
      var fresh = blank.cloneNode(true);
      each(fresh, 'input[type="text"]', function (box) { box.value = ""; box.defaultValue = ""; box.removeAttribute("data-saved"); });
      each(fresh, "[data-node-tls]", function (box) { box.checked = false; box.defaultChecked = false; });
      each(fresh, 'input[name$="_self_signed"]', function (box) { box.checked = true; box.defaultChecked = true; });
      fresh.open = true;
      rows.appendChild(fresh);
      renumber(rows);
      showSelfSigned(fresh);
      var address = fresh.querySelector('input[name$="_address"]');
      if (address) address.focus();
      return;
    }
    var discard = target.closest("[data-card-discard]");
    if (discard) { touched = true; discardCard(discard.closest("[data-card]")); update(); return; }
    if (target.closest("[data-discard-all]")) { event.preventDefault(); discardAll(); return; }
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
    if (show) {
      var card = document.getElementById("card-" + show.getAttribute("data-show-card"));
      if (!card) return;
      event.preventDefault();
      var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      card.scrollIntoView({ block: "start", behavior: reduce ? "auto" : "smooth" });
      var problem = card.querySelector('[aria-invalid="true"], .setting-problem');
      var row = problem && problem.closest("details");
      if (row) row.open = true;
      var control = (problem && problem.closest(".setting-field") || card).querySelector("input:not([disabled]):not([type=hidden]), select, textarea");
      if (control) control.focus({ preventScroll: true });
    }
  });

  // ---- typing and ticking ----------------------------------------------

  function changedHere(event) {
    if (!event.target.closest || !event.target.closest("#settings-form")) return;
    if (event.target.matches("[data-node-tls]")) showSelfSigned(event.target.closest("[data-node-row]"));
    if (event.target.name === "key_custody.enabled_backends") showCustodyBackends(document);
    touched = true;
    update();
  }
  document.addEventListener("input", changedHere);
  document.addEventListener("change", changedHere);

  // ---- saving -----------------------------------------------------------

  // Before fixi (capture): confirm a save that leaves a network stores use
  // with no node. Cancelling stops fixi too (static/fx-glue.js).
  document.addEventListener("submit", function (event) {
    if (event.target.id !== "settings-form") return;
    var cards = event.target.querySelectorAll("[data-network][data-tenant-count]");
    for (var i = 0; i < cards.length; i++) {
      var card = cards[i];
      var stores = Number(card.getAttribute("data-tenant-count")) || 0;
      var before = Number(card.getAttribute("data-saved-count")) || 0;
      var now = 0;
      each(card, '[data-node-row] input[name$="_address"]', function (box) { if (box.value.trim() !== "") now++; });
      if (stores > 0 && before > 0 && now === 0) {
        var network = card.getAttribute("data-network");
        var use = stores === 1 ? "1 store uses" : stores + " stores use";
        if (!window.confirm(use + " the " + network + " network. Without a node, their payments won't be detected. Save anyway?")) {
          event.preventDefault();
          goAfterSave = null;
          return;
        }
      }
    }
    submitting = true;
  }, true);

  window.addEventListener("beforeunload", function (event) {
    if (!submitting && !reloading && update() > 0) {
      event.preventDefault();
      event.returnValue = "";
    }
  });

  // ---- after each swap --------------------------------------------------

  function settle() {
    submitting = false;
    touched = false;
    leavingTo = null;
    listsAtLoad = {};
    each(document, "[data-network][data-card]", function (card) {
      var list = card.querySelector("[data-node-rows]");
      if (list) listsAtLoad[card.id] = list.cloneNode(true);
    });
    var box = bar();
    var message = box && box.querySelector("[data-save-bar-message]");
    barAtLoad = message
      ? { html: message.innerHTML, failed: box.classList.contains("is-failed"), cards: document.querySelectorAll("[data-card].is-failed").length }
      : null;
    showSelfSigned(document);
    showCustodyBackends(document);
    update();
    if (goAfterSave) {
      var href = goAfterSave;
      goAfterSave = null;
      // On "fx:settled", the glue has put the new tab bar in, so the link
      // clicked is the one that stays (one it replaced mid-request would
      // never say its request finished).
      var refused = document.querySelector("[data-card].is-failed, #save-bar.is-failed");
      if (!refused) go(href);
    }
  }

  // A save that comes back without a panel to swap in (a dropped
  // connection, or any failure but 422, which fixi's glue doesn't swap):
  // the page guards its unsaved changes again, and doesn't go anywhere.
  // fixi says "finally" before it swaps, so a save that will swap is left to
  // `settle`.
  document.addEventListener("fx:finally", function (event) {
    if (!event.target.closest || !event.target.closest("#settings-form")) return;
    var response = event.detail && event.detail.cfg && event.detail.cfg.response;
    if (response && (response.ok || response.status === 422)) return;
    submitting = false;
    goAfterSave = null;
  });

  // After fixi's glue has finished with a swap (fx-glue.js "fx:settled"):
  // the panel, and the tab bar and banners it brought, are all in place.
  document.addEventListener("fx:settled", settle);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", settle);
  else settle();
})();

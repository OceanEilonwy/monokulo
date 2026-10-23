// The admin settings page's Monero nodes, with JavaScript
// (crates/monokulo/src/views/admin.rs). The page's cards, save bar and
// toasts are the settings components (the ui crate's islands); without
// either script the page already works: rows open to be edited, Move up and
// Move down reorder, and each button saves at once. This adds:
//   - node rows, reordered by their sortable list (dragged by their handle,
//     or its arrow keys), are named and numbered for their new places, and
//     Remove takes a row out; both are unsaved changes like any other, and
//     a new order counts as one change however many rows it moves;
//   - "Add another" adds a blank row;
//   - a row's self-signed box shows only while its Use TLS box is ticked;
//   - the question (a ui dialog) before a save leaves a network stores use with no
//     node;
//   - a setting that needs a switch (`data-needs-switch`, the switch's id:
//     the instance's own rate limit) is enabled as the switch turns on and
//     disabled as it turns off. Without the script it is enabled by saving
//     with the switch on.
// Everything listens on the document, so it keeps working on the panel
// fixi swaps in after a save or a tab link.
(function () {
  "use strict";

  var listsAtLoad = {};

  function each(root, selector, fn) { Array.prototype.forEach.call(root.querySelectorAll(selector), fn); }

  function ticks(el) { return el.type === "checkbox" || el.type === "radio"; }

  // What a row's control holds, against what's saved: `data-saved` on a
  // row a refused save shows again, its default value otherwise.
  function edited(el) {
    var now = ticks(el) ? el.checked : el.value;
    if (el.hasAttribute("data-saved")) {
      var saved = el.getAttribute("data-saved");
      return now !== (ticks(el) ? saved === "on" : saved);
    }
    return now !== (ticks(el) ? el.defaultChecked : el.defaultValue);
  }

  function rowEdited(row) {
    return Array.prototype.some.call(row.querySelectorAll("input[name]"), edited);
  }

  function savedIndex(row) {
    var at = row.getAttribute("data-node-saved");
    return at === null ? null : Number(at);
  }

  // A network's changes: rows edited, added or taken out, and one more
  // for a new order of the rows that were there. Counted with the card's
  // own settings (the settings card's behaviour asks with "mk-count").
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

  document.addEventListener("mk-count", function (event) {
    if (event.target.hasAttribute("data-network")) event.detail.changes += networkChanges(event.target);
  }, true);

  // Discard on a network: its rows as they came. A network a save refused
  // shows the rows as sent, and its saved rows are only on the server: the
  // tab is loaded again.
  document.addEventListener("mk-discard", function (event) {
    var card = event.target;
    if (!card.hasAttribute("data-network")) return;
    if (card.classList.contains("is-failed") || !listsAtLoad[card.id]) {
      event.preventDefault();
      var tab = document.getElementById("settings-panel");
      var base = document.querySelector("meta[name=\"monokulo-base\"]");
      location.href = (base ? base.content : "") + "/admin/settings?tab=" + (tab ? tab.getAttribute("data-tab") : "nodes");
      return;
    }
    card.querySelector("[data-node-rows]").replaceWith(listsAtLoad[card.id].cloneNode(true));
    showSelfSigned(card);
  }, true);

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

  // The settings form hears that something changed which isn't a
  // setting's control (a row moved, added or taken out).
  function changed() {
    var form = document.getElementById("settings-form");
    if (form) form.dispatchEvent(new CustomEvent("mk-changed"));
  }

  // ---- reordering node rows --------------------------------------------

  // Each list is a ui sortable list: its island drags a row by its handle
  // (or moves it with the handle's arrow keys), the rows it passes sliding
  // out of its way. Each place it takes renames the rows' fields and says
  // their places again at once; a new order once settled is a change.
  document.addEventListener("mk-reorder", function (event) {
    if (event.target.hasAttribute("data-node-rows")) renumber(event.target);
  });

  document.addEventListener("mk-reordered", function (event) {
    if (!event.target.hasAttribute("data-node-rows")) return;
    event.detail.item.setAttribute("data-node-moved", "");
    changed();
  });

  // ---- clicks -----------------------------------------------------------

  document.addEventListener("click", function (event) {
    var target = event.target.closest ? event.target : null;
    if (!target) return;
    var remove = target.closest("[data-node-remove]");
    if (remove) {
      event.preventDefault();
      var row = remove.closest("[data-node-row]");
      var list = row.parentNode;
      row.remove();
      renumber(list);
      changed();
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
      fresh.querySelector("details").open = true;
      rows.appendChild(fresh);
      renumber(rows);
      showSelfSigned(fresh);
      var address = fresh.querySelector('input[name$="_address"]');
      if (address) address.focus();
    }
  });

  // Each control that needs a switch, enabled exactly while it is on.
  function followSwitches(root) {
    each(root, "[data-needs-switch]", function (control) {
      var on = document.getElementById(control.getAttribute("data-needs-switch"));
      if (on) control.disabled = !on.checked;
    });
  }

  document.addEventListener("change", function (event) {
    if (event.target.matches && event.target.matches("[data-node-tls]")) showSelfSigned(event.target.closest("[data-node-row]"));
    if (event.target.matches && event.target.matches('input[role="switch"]')) followSwitches(document);
  });

  // Discard puts the switch back as it was saved: once the card's own
  // handler has done so, the controls that need it follow.
  document.addEventListener("mk-discard", function () { setTimeout(function () { followSwitches(document); }, 0); });

  // ---- saving -----------------------------------------------------------

  // Before fixi and the settings form (capture): a save that leaves a
  // network stores use with no node asks first, in #node-clear-dialog.
  // Keep editing stops it; Save anyway sends it.
  var askedAlready = false;
  document.addEventListener("submit", function (event) {
    if (event.target.id !== "settings-form") return;
    if (askedAlready) { askedAlready = false; return; }
    var cards = event.target.querySelectorAll("[data-network][data-tenant-count]");
    for (var i = 0; i < cards.length; i++) {
      var card = cards[i];
      var stores = Number(card.getAttribute("data-tenant-count")) || 0;
      var before = Number(card.getAttribute("data-saved-count")) || 0;
      var now = 0;
      each(card, '[data-node-row] input[name$="_address"]', function (box) { if (box.value.trim() !== "") now++; });
      if (stores > 0 && before > 0 && now === 0) {
        var dialog = document.getElementById("node-clear-dialog");
        if (!dialog || typeof dialog.showModal !== "function") return;
        event.preventDefault();
        var network = card.getAttribute("data-network");
        var use = stores === 1 ? "1 store uses" : stores + " stores use";
        dialog.querySelector("[data-node-clear-text]").textContent =
          use + " the " + network + " network. Without a node, their payments won't be detected.";
        dialog.showModal();
        return;
      }
    }
  }, true);

  document.addEventListener("click", function (event) {
    var go = event.target.closest && event.target.closest("[data-node-clear-go]");
    if (!go) return;
    var form = document.getElementById("settings-form");
    go.closest("dialog").close();
    if (!form) return;
    askedAlready = true;
    form.requestSubmit(form.querySelector("[data-save]") || undefined);
  });

  // ---- after each swap --------------------------------------------------

  function settle() {
    listsAtLoad = {};
    each(document, "[data-network]", function (card) {
      var list = card.querySelector("[data-node-rows]");
      if (list) listsAtLoad[card.id] = list.cloneNode(true);
    });
    showSelfSigned(document);
    followSwitches(document);
  }

  // After fixi's glue has finished with a swap (fx-glue.js "fx:settled").
  document.addEventListener("fx:settled", settle);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", settle);
  else settle();
})();

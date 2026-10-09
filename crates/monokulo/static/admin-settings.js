// The admin settings page's Monero nodes, with JavaScript
// (crates/monokulo/src/views/admin.rs). The page's cards, save bar and
// toasts are the settings components (static/settings-form.js); without
// either script the page already works: rows open to be edited, Move up and
// Move down reorder, and each button saves at once. This adds:
//   - node rows reorder by dragging their handle (or its arrow keys), and
//     Remove takes a row out; both are unsaved changes like any other, and
//     a new order counts as one change however many rows it moves;
//   - "Add another" adds a blank row;
//   - a row's self-signed box shows only while its Use TLS box is ticked;
//   - the confirmation before a save leaves a network stores use with no
//     node.
// Everything listens on the document, so it keeps working on the panel
// fixi swaps in after a save or a tab link.
(function () {
  "use strict";

  var listsAtLoad = {};

  function each(root, selector, fn) { Array.prototype.forEach.call(root.querySelectorAll(selector), fn); }
  function settings() { return document.querySelector("mk-settings-form"); }

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
  // own settings (settings-form.js asks with "mk-count").
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
      location.href = "/dashboard/admin/settings?tab=" + (tab ? tab.getAttribute("data-tab") : "nodes");
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

  function changed() {
    var form = settings();
    if (form && form.changed) form.changed();
  }

  // ---- reordering node rows --------------------------------------------

  var dragging = null;

  function moveRow(row, before) {
    var list = row.parentNode;
    list.insertBefore(row, before);
    row.setAttribute("data-node-moved", "");
    renumber(list);
    changed();
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
      fresh.open = true;
      rows.appendChild(fresh);
      renumber(rows);
      showSelfSigned(fresh);
      var address = fresh.querySelector('input[name$="_address"]');
      if (address) address.focus();
    }
  });

  document.addEventListener("change", function (event) {
    if (event.target.matches && event.target.matches("[data-node-tls]")) showSelfSigned(event.target.closest("[data-node-row]"));
  });

  // ---- saving -----------------------------------------------------------

  // Before fixi and the settings form (capture): confirm a save that
  // leaves a network stores use with no node. Cancelling stops both.
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
          return;
        }
      }
    }
  }, true);

  // ---- after each swap --------------------------------------------------

  function settle() {
    listsAtLoad = {};
    each(document, "[data-network]", function (card) {
      var list = card.querySelector("[data-node-rows]");
      if (list) listsAtLoad[card.id] = list.cloneNode(true);
    });
    showSelfSigned(document);
  }

  // After fixi's glue has finished with a swap (fx-glue.js "fx:settled").
  document.addEventListener("fx:settled", settle);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", settle);
  else settle();
})();

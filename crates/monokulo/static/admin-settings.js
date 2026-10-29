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
//   - the Push relay card's apps: Remove and Add an app act in place;
//   - after a save the address bar says whether the Push relay card is
//     opened (?relay=edit) as the page now shows it, so a reload shows
//     the same;
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
      // A switch's listing names it by its value.
      each(row, 'input[name="switches"]', function (listing) {
        listing.value = listing.value.replace(names, "node_" + network + "_" + i + "_");
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

  // ---- the Push relay card's apps ----------------------------------------
  //
  // Without the script Remove and Add an app post the tab and come back
  // with the rows changed, nothing saved. With it they act in place: Remove
  // takes the row out, Add an app adds a blank one from the template; the
  // rows are numbered again for their places, and each is a change counted
  // with the card's own settings, saved with the bar.

  var appsAtLoad = null;

  function renumberApps(list) {
    var names = /relay_app_(\d+|__i__)_/g, ids = /relay-app-(\d+|__i__)-/g;
    each(list, ":scope > [data-relay-app]", function (row, i) {
      row.setAttribute("data-relay-app", String(i));
      each(row, "[name],[id],[for],[aria-describedby]", function (el) {
        ["name", "id", "for", "aria-describedby"].forEach(function (attr) {
          if (!el.hasAttribute(attr)) return;
          el.setAttribute(attr, el.getAttribute(attr).replace(names, "relay_app_" + i + "_").replace(ids, "relay-app-" + i + "-"));
        });
      });
      each(row, "[data-relay-app-remove]", function (button) { button.value = "remove-app:" + i; });
    });
  }

  // Saved apps taken out, rows added with something in them, saved rows
  // edited: one change each.
  function appChanges(list) {
    var rows = Array.prototype.slice.call(list.querySelectorAll(":scope > [data-relay-app]"));
    var saved = rows.filter(function (row) { return row.hasAttribute("data-relay-app-saved"); });
    var count = Math.max(0, (Number(list.getAttribute("data-saved-count")) || 0) - saved.length);
    rows.forEach(function (row) {
      var boxes = Array.prototype.slice.call(row.querySelectorAll("input[name]"));
      var text = row.hasAttribute("data-relay-app-saved")
        ? rowEdited(row)
        : boxes.some(function (box) { return box.value.trim() !== ""; });
      if (text) count++;
      row.classList.toggle("is-changed", text);
    });
    return count;
  }

  document.addEventListener("mk-count", function (event) {
    var list = event.target.querySelector && event.target.querySelector("[data-relay-apps]");
    if (list) event.detail.changes += appChanges(list);
  }, true);

  // Discard: the rows as they came; a refused save's rows (as sent) only
  // the server has as saved, so the tab is loaded again.
  document.addEventListener("mk-discard", function (event) {
    var list = event.target.querySelector && event.target.querySelector("[data-relay-apps]");
    if (!list) return;
    if (event.target.classList.contains("is-failed") || !appsAtLoad || list.querySelector("[data-saved]")) {
      event.preventDefault();
      var base = document.querySelector("meta[name=\"monokulo-base\"]");
      location.href = (base ? base.content : "") + "/admin/settings?tab=server&relay=edit#card-relay";
      return;
    }
    list.replaceWith(appsAtLoad.cloneNode(true));
  }, true);

  document.addEventListener("click", function (event) {
    var target = event.target.closest ? event.target : null;
    if (!target) return;
    var remove = target.closest("[data-relay-app-remove]");
    var add = !remove && target.closest("[data-relay-app-add]");
    var list = (remove || add) && (remove || add).closest("[data-relay-apps]");
    if (!list) return;
    event.preventDefault();
    if (remove) {
      remove.closest("[data-relay-app]").remove();
    } else {
      var template = list.querySelector("template[data-relay-app-template]");
      if (!template) return;
      var fresh = template.content.firstElementChild.cloneNode(true);
      list.insertBefore(fresh, template);
    }
    renumberApps(list);
    changed();
    if (add) {
      var name = list.querySelector(":scope > [data-relay-app]:last-of-type input");
      if (name) name.focus();
    }
  });

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
    var apps = document.querySelector("[data-relay-apps]");
    appsAtLoad = apps ? apps.cloneNode(true) : null;
    showSelfSigned(document);
    followSwitches(document);
    followRelayCard();
  }

  // A saved change closes the Push relay card, a refused one keeps it
  // opened; the save is a POST, so fixi doesn't change the address
  // (fx-push-url is for links). Same history entry, its fx state kept.
  function followRelayCard() {
    var card = document.getElementById("card-relay");
    if (!card || !window.history || !history.replaceState) return;
    var url = new URL(location.href);
    var open = card.classList.contains("is-open");
    if (open === (url.searchParams.get("relay") === "edit")) return;
    if (open) url.searchParams.set("relay", "edit");
    else url.searchParams.delete("relay");
    url.hash = "card-relay";
    history.replaceState(history.state, "", url.href);
  }

  // After fixi's glue has finished with a swap (fx-glue.js "fx:settled").
  document.addEventListener("fx:settled", settle);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", settle);
  else settle();
})();

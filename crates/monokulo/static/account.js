// The account page's email question (crates/monokulo/src/views/account.rs).
// Without JavaScript, saving a new email answers with a page asking for it
// again. With it, Save opens that question as a dialog instead: the new
// address and the old, and the new one typed again. Once they match, the
// form is sent with it, and the save goes straight through.
//
// Loaded before the settings components (static/settings-form.js): a save
// this holds back never reaches them, so the page still guards the unsaved
// change.
(function () {
  "use strict";

  function norm(value) { return value.trim().toLowerCase(); }

  // Whether the profile form's email differs from the one saved (the
  // box's data-saved after a refused save, its default value otherwise).
  function newEmail(form) {
    var box = form.querySelector("[data-email-input]");
    if (!box) return null;
    var saved = box.hasAttribute("data-saved") ? box.getAttribute("data-saved") : box.defaultValue;
    return norm(box.value) !== norm(saved) ? norm(box.value) : null;
  }

  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (form.id !== "settings-form") return;
    var dialog = form.querySelector("#email-confirm");
    var email = newEmail(form);
    if (!dialog || !email || typeof dialog.showModal !== "function") return;
    var again = dialog.querySelector("[data-email-again]");
    if (norm(again.value) === email) return;
    // Not yet asked (or asked about another address): ask first.
    event.preventDefault();
    event.stopImmediatePropagation();
    dialog.querySelector("[data-email-new]").textContent = email;
    again.value = "";
    dialog.querySelector("[data-email-mismatch]").hidden = true;
    again.removeAttribute("aria-invalid");
    dialog.showModal();
    again.focus();
  }, true);

  document.addEventListener("click", function (event) {
    var target = event.target.closest ? event.target : null;
    if (!target) return;
    var dialog = target.closest("#email-confirm");
    if (!dialog) return;
    var again = dialog.querySelector("[data-email-again]");
    if (target.closest("[data-email-cancel]")) {
      again.value = "";
      dialog.close();
      return;
    }
    if (target.closest("[data-email-go]")) {
      var form = dialog.closest("form");
      if (norm(again.value) !== newEmail(form)) {
        dialog.querySelector("[data-email-mismatch]").hidden = false;
        again.setAttribute("aria-invalid", "true");
        again.focus();
        return;
      }
      dialog.close();
      form.requestSubmit(form.querySelector("[data-save]") || undefined);
    }
  });

  // Enter in the box means Change email, not the form's Save.
  document.addEventListener("keydown", function (event) {
    if (event.key !== "Enter" || !event.target.matches || !event.target.matches("#email-confirm [data-email-again]")) return;
    event.preventDefault();
    event.target.closest("#email-confirm").querySelector("[data-email-go]").click();
  });

  // Closed with Escape: the box is emptied, so the next Save asks again.
  document.addEventListener("close", function (event) {
    if (event.target.id !== "email-confirm") return;
    var again = event.target.querySelector("[data-email-again]");
    var form = event.target.closest("form");
    if (form && norm(again.value) !== newEmail(form)) again.value = "";
  }, true);
})();

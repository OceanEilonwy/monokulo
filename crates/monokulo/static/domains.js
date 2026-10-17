// A store's domains (views/domains.rs): while one is still waiting for its
// DNS record, its Check again form (data-recheck) is sent again every 30
// seconds, as long as the page is in view. fixi swaps the answer in place
// (the card, or the check page's result); a verified domain's answer has no
// such form, so the checking stops by itself.
(function () {
  // A second over the 30 the server asks between checks of one domain.
  var EVERY_MS = 31000;
  setInterval(function () {
    if (document.visibilityState !== 'visible') return;
    document.querySelectorAll('form[data-recheck]').forEach(function (form) {
      // One check at a time per form: fixi marks its target busy.
      var target = document.querySelector(form.getAttribute('fx-target'));
      if (target && target.getAttribute('aria-busy') === 'true') return;
      form.requestSubmit();
    });
  }, EVERY_MS);
})();

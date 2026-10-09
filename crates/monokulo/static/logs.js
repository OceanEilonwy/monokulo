(function () {
  var MAX_ROWS = 1000, live = null;
  function button() { return document.getElementById("log-live"); }
  function tailUrl() { var rows = document.getElementById("log-rows"); return rows && rows.dataset.tailUrl; }
  // Pause shows the moment Live is pressed, not when the stream answers.
  function show(on) {
    var b = button();
    if (!b) return;
    b.setAttribute("aria-pressed", on ? "true" : "false");
    b.querySelector(".log-live-label").textContent = on ? "Pause" : "Live";
    b.disabled = !on && !tailUrl();
  }
  function stop() {
    var cfg = live;
    live = null;
    if (cfg) { if (cfg.sse) cfg.sse.close(); else cfg.abort(); }
    show(false);
  }
  document.addEventListener("fx:config", function (evt) {
    // A line's properties load the first time it opens, never on close.
    if (evt.target.matches && evt.target.matches("details.log-row")) {
      if (!evt.target.open || evt.target.dataset.loaded) evt.preventDefault();
      else evt.target.dataset.loaded = "1";
      return;
    }
    if (evt.target.id === "log-live") {
      // Live follows the search shown: its stream starts after the newest
      // line the results hold.
      if (live || !tailUrl()) { evt.preventDefault(); stop(); return; }
      evt.detail.cfg.action = tailUrl();
      live = evt.detail.cfg;
      show(true);
    } else if (evt.target.id === "log-search") stop();
  });
  // The stream ended for good (or never opened).
  document.addEventListener("fx:finally", function (evt) {
    if (evt.target.id === "log-live" && live === evt.detail.cfg) { live = null; show(false); }
  });
  // New results: Live starts after their newest line, or not at all on an
  // older page.
  document.addEventListener("fx:swapped", function () { if (!live) show(false); });
  document.addEventListener("fx:sse:swapped", function () {
    var rows = document.querySelectorAll("#log-rows > .log-row");
    for (var i = MAX_ROWS; i < rows.length; i++) rows[i].remove();
  });
  // Syntax opens its help as a dialog instead of following the link to the
  // same help as a page. Use puts an example in the box; a name chip adds
  // that name to it.
  var help = document.getElementById("query-help"), helpLink = document.getElementById("query-help-link");
  var box = document.getElementById("log-q");
  if (help && helpLink && box && typeof help.showModal === "function") {
    helpLink.addEventListener("click", function (evt) { evt.preventDefault(); help.showModal(); });
    help.addEventListener("click", function (evt) {
      if (evt.target === help) { help.close(); return; }
      var use = evt.target.closest(".qh-use"), chip = evt.target.closest(".qh-chip");
      if (!use && !chip) return;
      evt.preventDefault();
      box.value = use ? use.dataset.q : (box.value.trim() ? box.value.trim() + " and " : "") + chip.textContent + " ";
      help.close();
      box.focus();
    });
  }
})();

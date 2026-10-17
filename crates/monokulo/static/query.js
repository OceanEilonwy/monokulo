// The Query page (views::query), once script runs. Without it every part
// still works: Run and Go submit, a long value opens on a page of its own.
//
// - Ctrl/⌘+Enter in the editor runs the statement.
// - The table filter narrows the list as it is typed in.
// - Copy buttons in the grid copy their value: a long text's whole value
//   (the cell holds it, cut short only on screen), or a blob's hex.
// - On a phone the table list starts folded behind "Tables (N)".
(function () {
  document.addEventListener("keydown", function (evt) {
    if (evt.key !== "Enter" || !(evt.ctrlKey || evt.metaKey)) return;
    var editor = evt.target.closest && evt.target.closest(".query-editor");
    if (!editor) return;
    evt.preventDefault();
    editor.requestSubmit();
  });

  document.addEventListener("input", function (evt) {
    if (!evt.target.matches || !evt.target.matches("[data-table-filter]")) return;
    var words = evt.target.value.trim().toLowerCase();
    var browser = evt.target.closest(".mk-table-browser");
    if (!browser) return;
    browser.querySelectorAll("[data-table]").forEach(function (table) {
      table.hidden = words !== "" && table.dataset.table.toLowerCase().indexOf(words) === -1;
    });
  });

  document.addEventListener("click", function (evt) {
    var button = evt.target.closest && evt.target.closest("[data-copy-cell], [data-copy]");
    if (!button || !button.closest(".mk-data-grid")) return;
    var text = button.hasAttribute("data-copy")
      ? button.getAttribute("data-copy")
      : button.closest(".mk-data-long").querySelector(".mk-data-value").textContent;
    if (navigator.clipboard) navigator.clipboard.writeText(text);
  });

  function fold() {
    if (!window.matchMedia("(max-width: 640px)").matches) return;
    document.querySelectorAll("details.query-tables[open]").forEach(function (d) { d.open = false; });
  }
  fold();
  // Again when the other database's tables arrive, not after each run.
  document.addEventListener("fx:settled", function (evt) {
    var target = evt.detail && evt.detail.cfg && evt.detail.cfg.target;
    if (target && target.id === "query-workspace") fold();
  });
})();

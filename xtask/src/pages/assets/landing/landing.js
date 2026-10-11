document.documentElement.classList.add("js");

// Each install tablist works independently. Without JavaScript all panels show.
document.querySelectorAll("[role=tablist]").forEach(function (list) {
  var tabs = Array.prototype.slice.call(list.querySelectorAll("[role=tab]"));
  if (!tabs.length) return;
  function select(tab, focus) {
    tabs.forEach(function (t) {
      var on = t === tab;
      t.setAttribute("aria-selected", on ? "true" : "false");
      t.tabIndex = on ? 0 : -1;
      document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
    });
    if (focus) tab.focus();
  }
  tabs.forEach(function (t, i) {
    t.addEventListener("click", function () { select(t); });
    t.addEventListener("keydown", function (e) {
      var d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
      if (d) { e.preventDefault(); select(tabs[(i + d + tabs.length) % tabs.length], true); }
      if (e.key === "Home" || e.key === "End") {
        e.preventDefault(); select(tabs[e.key === "Home" ? 0 : tabs.length - 1], true);
      }
    });
  });
  list.hidden = false;
  var initial = tabs.filter(function (t) {
    return location.hash === "#" + t.id.replace("tab-", "") ||
      location.hash === "#" + t.getAttribute("aria-controls");
  })[0];
  select(initial || tabs[0]);
});

// Copy buttons on every command and key (comments after a command are left out).
document.querySelectorAll(".cmd").forEach(function (box) {
  var b = document.createElement("button");
  b.type = "button"; b.className = "copy"; b.textContent = "Copy";
  b.setAttribute("aria-label", "Copy to clipboard");
  b.addEventListener("click", function () {
    var text = box.querySelector("pre").textContent.replace(/[ \t]+#.*$/gm, "");
    navigator.clipboard.writeText(text).then(function () {
      b.textContent = "Copied!";
      setTimeout(function () { b.textContent = "Copy"; }, 1600);
    });
  });
  box.appendChild(b);
});

// One OpenWrt version at a time. Without JavaScript every version's blocks
// show, each under its own label.
(function () {
  var pick = document.getElementById("openwrt-target");
  if (!pick) return;
  function show() {
    document.querySelectorAll(".target").forEach(function (el) {
      el.hidden = el.getAttribute("data-target") !== pick.value;
    });
    document.querySelectorAll(".target-label").forEach(function (el) { el.hidden = true; });
  }
  pick.addEventListener("change", show);
  pick.parentNode.hidden = false;
  show();
})();

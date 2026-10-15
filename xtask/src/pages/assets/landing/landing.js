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

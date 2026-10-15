document.documentElement.classList.add("js");

// Copy buttons on every command and key (comments after a command are left out).
document.querySelectorAll(".cmd").forEach(function (box) {
  var b = document.createElement("button");
  b.type = "button"; b.className = "mk-btn copy"; b.dataset.size = "compact"; b.textContent = "Copy";
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

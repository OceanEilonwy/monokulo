// What fixi and ssexi leave out on purpose, for monokulo's pages
// (structured_logging.md 4.3). Every page works without any of this; it
// only makes the fixi-enhanced parts behave like ordinary pages would.
//
// Attributes it understands, on an element with fx-action:
//   fx-push-url      after a successful swap, put the request's URL in the
//                    address bar (GET only), so back/forward and sharing
//                    work; back/forward then reloads the page.
//   fx-replace       abort a request still in flight and send this one
//                    (fixi's default drops the new one instead).
//   fx-debounce="ms" wait this long for the trigger to go quiet first.
//   fx-own-errors    the page's script shows this element's failures;
//                    no banner.
//   fx-sse-reconnect reconnect a dropped event stream (ssexi), sending
//                    Last-Event-ID, and pause it while the tab is hidden;
//                    a named "done" event ends it for good. While a stream
//                    is open, the page's Reload button (.reload) is hidden.
// and on a form:
//   fx-submit-on-change  submit when a select, checkbox or date changes.
//
// Everywhere:
//   - the target is marked aria-busy while a request runs;
//   - forms post urlencoded, as without JavaScript;
//   - a failed request (a network error, or any status but 2xx and 422)
//     shows a banner instead of swapping in an error page;
//   - an element marked data-fx-oob in a response replaces the page's
//     element with the same id (fixi itself swaps one target);
//   - a submit another listener cancelled isn't sent;
//   - after a swap, the element marked data-fx-focus (an error, or the
//     section's heading) gets focus, so keyboard and screen reader users
//     land on the result;
//   - once a swap is done with all of that (its out-of-band parts in place,
//     focus moved), "fx:settled" is sent on the document, so a page's own
//     script can act on the page as it now is (fixi's "fx:swapped" comes
//     before this glue has put those parts in);
//   - every request says the browser's time zone (X-Timezone, and a tz
//     cookie for full page loads), so the server formats times in it;
//   - <html> gets class "js", so CSS can hide controls that need it.
(function () {
  "use strict";

  // Lets CSS show controls that only work with JavaScript.
  document.documentElement.classList.add("js");

  var zone = "";
  try { zone = Intl.DateTimeFormat().resolvedOptions().timeZone || ""; } catch (e) {}
  // Also as a cookie, so full page loads show times in it too.
  if (zone) document.cookie = "tz=" + encodeURIComponent(zone) + "; path=/; max-age=31536000; SameSite=Lax";
  // No view transitions: they flash on every small section swap.
  window.fixiCfg = { transition: false, headers: zone ? { "X-Timezone": zone } : {} };

  function banner(message) {
    var box = document.getElementById("fx-error");
    if (!box) {
      box = document.createElement("p");
      box.id = "fx-error";
      box.className = "error fx-error";
      box.setAttribute("role", "alert");
      document.body.insertBefore(box, document.body.firstChild);
    }
    box.textContent = message;
    box.hidden = false;
  }

  function clearBanner() {
    var box = document.getElementById("fx-error");
    if (box) box.hidden = true;
  }

  document.addEventListener("fx:config", function (evt) {
    var elt = evt.target, cfg = evt.detail.cfg;
    // A submit another script cancelled (a confirm() the user declined)
    // isn't sent; fixi itself doesn't look.
    if (cfg.trigger && cfg.trigger.type === "submit" && cfg.trigger.defaultPrevented) {
      evt.preventDefault();
      return;
    }
    if (elt.hasAttribute("fx-replace")) {
      evt.detail.requests.forEach(function (other) { other.abort(); });
      cfg.drop = 0;
    }
    // Forms post as application/x-www-form-urlencoded, as they do without
    // JavaScript (fixi would send FormData as multipart), unless the form
    // asks for multipart itself.
    var form = elt.form || elt.closest("form");
    if (cfg.body instanceof FormData && !(form && form.enctype === "multipart/form-data")) {
      cfg.body = new URLSearchParams(cfg.body);
    }
    var wait = parseInt(elt.getAttribute("fx-debounce"), 10);
    if (wait > 0) {
      var generation = (elt.__fxGeneration = (elt.__fxGeneration || 0) + 1);
      cfg.confirm = function () {
        return new Promise(function (resolve) {
          setTimeout(function () { resolve(elt.__fxGeneration === generation); }, wait);
        });
      };
    }
    if (elt.hasAttribute("fx-sse-reconnect")) {
      cfg.sseReconnect = true;
      cfg.ssePauseOnHidden = true;
    }
  });

  // A form marked fx-submit-on-change searches as soon as a choice
  // changes (a select, a checkbox, a date), not only on its button. Text
  // boxes still wait for Enter.
  document.addEventListener("change", function (evt) {
    var form = evt.target.form;
    if (form && form.hasAttribute("fx-submit-on-change") && evt.target.matches("select, input[type=checkbox], input[type=radio], input[type=datetime-local]")) {
      form.requestSubmit();
    }
  });

  document.addEventListener("fx:before", function (evt) {
    evt.detail.cfg.target.setAttribute("aria-busy", "true");
  });

  document.addEventListener("fx:finally", function (evt) {
    evt.detail.cfg.target.removeAttribute("aria-busy");
  });

  document.addEventListener("fx:after", function (evt) {
    var cfg = evt.detail.cfg, response = cfg.response;
    // A 422 carries the section with its errors in it and is swapped like
    // a success. Anything else that isn't a success (a 5xx page, a 403
    // after the session ended) doesn't belong in a section.
    // fx-own-errors: the page's own script shows this element's failures.
    if (evt.target.hasAttribute("fx-own-errors")) return;
    if (response && !response.ok && response.status !== 422) {
      evt.preventDefault();
      banner(response.status === 401 || response.status === 403
        ? "You've been signed out. Reload the page to sign in again."
        : "Something went wrong (" + response.status + "). Reload the page to try again.");
      return;
    }
    clearBanner();
    if (evt.target.hasAttribute("fx-push-url") && cfg.method === "GET" && response && response.ok) {
      var url = new URL(cfg.action, location.href);
      if (url.href !== location.href) history.pushState({ fx: true }, "", url.href);
    }
  });

  document.addEventListener("fx:error", function (evt) {
    var error = evt.detail.error;
    if ((error && error.name === "AbortError") || evt.target.hasAttribute("fx-own-errors")) return;
    banner("Couldn't reach the server. Check your connection and try again.");
  });

  document.addEventListener("fx:swapped", function (evt) {
    // Out-of-band parts: an element marked data-fx-oob that came back with
    // a swap replaces the page's element with the same id, wherever that
    // is (a save in one section that changes another).
    document.querySelectorAll("[data-fx-oob]").forEach(function (fresh) {
      fresh.removeAttribute("data-fx-oob");
      document.querySelectorAll("#" + CSS.escape(fresh.id)).forEach(function (old) {
        if (old !== fresh) { old.replaceWith(fresh); }
      });
    });
    var focus = document.querySelector("[data-fx-focus]");
    if (focus) {
      if (!focus.hasAttribute("tabindex")) focus.setAttribute("tabindex", "-1");
      focus.removeAttribute("data-fx-focus");
      // Without scrolling: the swapped section stays where it was on screen.
      focus.focus({ preventScroll: true });
    }
    document.dispatchEvent(new CustomEvent("fx:settled", { detail: evt.detail }));
  });

  // A page streaming its own updates doesn't need its Reload button;
  // it comes back if the stream fails for good. A stream whose subject
  // can't change any more says "done", and isn't reconnected.
  document.addEventListener("fx:sse:open", function () {
    document.querySelectorAll(".reload").forEach(function (b) { b.hidden = true; });
  });
  document.addEventListener("fx:sse:error", function () {
    document.querySelectorAll(".reload").forEach(function (b) { b.hidden = false; });
  });
  document.addEventListener("fx:sse:done", function (evt) {
    evt.detail.cfg.sse.close();
  });

  // Pages whose state lives in the URL were changed by pushState; show the
  // one the address bar now names. Only entries this script made (or the
  // page it started on) are reloaded, not in-page #anchor jumps.
  document.addEventListener("DOMContentLoaded", function () {
    if (document.querySelector("[fx-push-url]")) history.replaceState({ fx: true }, "");
  });
  window.addEventListener("popstate", function (evt) {
    if (evt.state && evt.state.fx) location.reload();
  });
})();

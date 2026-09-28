// Browser problem reports (structured_logging.md 2.5).
//
// Sends uncaught errors, unhandled promise rejections and failed fixi
// requests to /telemetry/client (a keepalive fetch, so a report sent as the
// page unloads still goes), tagged with the
// trace of the request that rendered this page (its
// <meta name="traceparent">), so they appear next to the server's own
// lines for that page in the Logs view. Other scripts report their own
// problems (a dropped event stream, a QR camera failure) through
// window.monokuloTelemetry.report(kind, message).
//
// Nothing here is needed for the page to work, and nothing is sent unless
// something goes wrong. At most MAX_REPORTS per page view; a 429 (the
// server's limit on log reports) drops every report until its Retry-After
// has passed.
(function () {
  "use strict";
  var ENDPOINT = "/telemetry/client";
  var MAX_REPORTS = 10;
  var MAX_TEXT = 1000;
  var sent = 0;
  var pausedUntil = 0;
  var meta = document.querySelector('meta[name="traceparent"]');
  var traceparent = meta ? meta.getAttribute("content") : null;

  function clip(text) {
    text = String(text == null ? "" : text);
    return text.length > MAX_TEXT ? text.slice(0, MAX_TEXT) : text;
  }

  function report(kind, message, detail) {
    if (sent >= MAX_REPORTS || !window.fetch || Date.now() < pausedUntil) return;
    sent += 1;
    var body = JSON.stringify({
      traceparent: traceparent,
      kind: clip(kind),
      message: clip(message),
      detail: detail ? clip(detail) : undefined,
      page: location.pathname
    });
    try {
      fetch(ENDPOINT, { method: "POST", body: body, keepalive: true, credentials: "same-origin",
                        headers: { "Content-Type": "application/json" } })
        .then(function (response) {
          if (response.status !== 429) return;
          var seconds = parseInt(response.headers.get("Retry-After"), 10);
          pausedUntil = Date.now() + (seconds > 0 ? seconds : 60) * 1000;
        })
        .catch(function () {});
    } catch (e) {
      // Reporting must never be the thing that breaks a page.
    }
  }

  window.addEventListener("error", function (event) {
    var where = event.filename ? event.filename + ":" + event.lineno + ":" + event.colno : "";
    report("error", event.message || "script error", where);
  });

  window.addEventListener("unhandledrejection", function (event) {
    var reason = event.reason;
    report("unhandledrejection", reason && reason.message ? reason.message : String(reason), reason && reason.stack);
  });

  document.addEventListener("fx:error", function (event) {
    var error = event.detail && event.detail.error;
    report("fx:error", error && error.message ? error.message : "request failed", event.target && event.target.id);
  });

  window.monokuloTelemetry = { report: report };
})();

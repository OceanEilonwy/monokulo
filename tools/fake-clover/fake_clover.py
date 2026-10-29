#!/usr/bin/env python3
"""A stand-in Clover for running Monokular end to end on a developer's machine.

Only what the till and the Monokular Bell use: the PKCE sign-in for low-trust
apps (which approves at once), the merchant and its properties, tenders,
payments, orders and refunds, and webhooks into a Bell. One host plays both
Clover's web host (/oauth/v2/authorize) and its API host (/oauth/v2/token,
/oauth/v2/refresh, /v3). Ring up a sale with:

    curl -X POST localhost:8740/fake/sale -d '{"items":[["Flat white, oat milk",2,450],["Almond croissant",1,375]]}'

An item is [name, quantity, cents]: Clover repeats a line item per unit sold,
so a quantity of 2 is two line items. A fractional quantity, such as
["Granola, per 100 g", 2.5, 120], is one per-unit line item with unitQty 2500
(Clover's thousandths). "tender" is "Monero" (the tender the till created or
found, by its label) unless it says "Cash", Clover's system tender: a payment
the till must not pick up, which still rings the Bell, as the Bell can't tell.

With FAKE_CLOVER_WEBHOOK_URL (a Bell's bell.public_uri followed by
/v1/clover/webhook) and FAKE_CLOVER_AUTH_CODE (its
MONOKULAR_BELL_CLOVER_AUTH_CODE) set, each sale is sent there as a P: CREATE
webhook with X-Clover-Auth, and each refund as a P: UPDATE. The Bell's
bell.clover.application_id must be FAKE_CLOVER_APP_ID (default FAKECLOVERAPP).

The cashier's side and QA's switches:

    POST /fake/sale     {"items": [...], "tender": "Monero"|"Cash", "merchant_id"?}
    POST /fake/refund   {"payment_id": "..."}   the cashier refunding in Clover's Orders app
    POST /fake/verify   Clover's {"verificationCode"} handshake, sent with no auth header
    POST /fake/webhook  {"objectId": "O:...", "type": "DELETE", "merchant_id"?, "appId"?}
    POST /fake/expire-tokens                    every access token expires now
    POST /fake/revoke   {"merchant_id": "..."}  every access and refresh token of the merchant dies
    POST /fake/limit    {"count": 1, "retry_after": 2}  the next count /v3 requests are 429 with retry-after

Env: FAKE_CLOVER_PORT (8740), FAKE_CLOVER_ACCESS_TTL (seconds an access token
lives, 1800), FAKE_CLOVER_REFRESH_CAP (live refresh tokens per merchant, 3:
one more invalidates the oldest), FAKE_CLOVER_DENY_TENDER_WRITE=1 (every
POST .../tenders is 403, as for an app without Merchant write).

Standard library only. Not Clover: shapes follow its API reference
(docs.clover.com) closely enough for the till and the Bell, and nothing more.
Times are Unix milliseconds everywhere but the OAuth expirations, which are
Unix seconds, as Clover has them.
"""
import base64, hashlib, json, os, random, secrets, string, sys, threading, time, uuid
import urllib.error, urllib.parse, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

APP_ID = os.environ.get("FAKE_CLOVER_APP_ID", "FAKECLOVERAPP")
ACCESS_TTL = int(os.environ.get("FAKE_CLOVER_ACCESS_TTL", "1800"))
REFRESH_TTL = 365 * 86400
REFRESH_CAP = int(os.environ.get("FAKE_CLOVER_REFRESH_CAP", "3"))
DENY_TENDER_WRITE = os.environ.get("FAKE_CLOVER_DENY_TENDER_WRITE") == "1"
TAX_RATE = 0.08875
SYSTEM_LABEL_KEYS = {"com.clover.tender.cash", "com.clover.tender.credit_card", "com.clover.tender.debit_card",
                     "com.clover.tender.check", "com.clover.tender.external_payment",
                     "com.clover.tender.external_gift_card"}

MERCHANT_ID = "MCLOVERDEV001"
OTHER_MERCHANT_ID = "MCLOVERDEV002"
EMPLOYEE_ID = "ECLOVERDEV001"
DEVICE_ID = "DCLOVERDEV001"
# Each merchant's test API token, as Clover's dashboard issues one (Setup ›
# API Tokens): it never expires, and lets a script or a curl reach /v3, or
# register with a Bell, without the sign-in.
TEST_TOKENS = {"fake-clover-test-token-0001": MERCHANT_ID, "fake-clover-test-token-0002": OTHER_MERCHANT_ID}
MERCHANTS = {
    MERCHANT_ID: {"name": "The Bean Room", "currency": "USD", "timezone": "America/Los_Angeles",
                  "owner": {"id": "OCLOVERDEV001", "name": "Dana Bean", "email": "dana@bean-room.example"}},
    OTHER_MERCHANT_ID: {"name": "Second Shop", "currency": "USD", "timezone": "America/New_York",
                        "owner": {"id": "OCLOVERDEV002", "name": "Sam Second", "email": "sam@second.example"}},
}

lock = threading.Lock()
state = {
    "codes": {},      # code -> {challenge, client_id, merchant}
    "access": {},     # token -> {merchant, expires (s)}
    "refresh": {},    # token -> {merchant, expires (s), live}
    "refresh_order": {m: [] for m in MERCHANTS},  # live refresh tokens per merchant, oldest first
    "tenders": {m: [] for m in MERCHANTS},
    "payments": {m: [] for m in MERCHANTS},
    "orders": {m: {} for m in MERCHANTS},
    "refunds": {m: [] for m in MERCHANTS},
    "limit": {"count": 0, "retry_after": 1},  # /fake/limit: the next requests answered 429
}

# Clover returns at most this many elements of a nested collection (an
# order's line items, a payment's refunds) when it is expanded.
NESTED_MAX = 100


def new_id():
    return "".join(random.choice(string.ascii_uppercase + string.digits) for _ in range(13))


def ms():
    return int(time.time() * 1000)


def log(message):
    sys.stderr.write("clover: " + message + "\n")


for merchant in MERCHANTS:
    state["tenders"][merchant] = [
        {"id": new_id(), "label": "Cash", "labelKey": "com.clover.tender.cash", "enabled": True, "visible": True,
         "opensCashDrawer": True, "supportsTipping": False, "editable": False},
        {"id": new_id(), "label": "Credit Card", "labelKey": "com.clover.tender.credit_card", "enabled": True,
         "visible": True, "opensCashDrawer": False, "supportsTipping": True, "editable": False},
    ]


def issue_tokens(merchant):
    """A new access token and refresh token for `merchant`: the oldest live refresh tokens past the cap die (R1)."""
    now = int(time.time())
    access, refresh = "clv-a-" + secrets.token_hex(12), "clv-r-" + secrets.token_hex(12)
    state["access"][access] = {"merchant": merchant, "expires": now + ACCESS_TTL}
    state["refresh"][refresh] = {"merchant": merchant, "expires": now + REFRESH_TTL, "live": True}
    order = state["refresh_order"][merchant]
    order.append(refresh)
    while len(order) > REFRESH_CAP:
        oldest = order.pop(0)
        state["refresh"][oldest]["live"] = False
        log("refresh token cap (%d) reached for %s: the oldest refresh token is dead" % (REFRESH_CAP, merchant))
    return {"access_token": access, "access_token_expiration": now + ACCESS_TTL,
            "refresh_token": refresh, "refresh_token_expiration": now + REFRESH_TTL}


def token_merchant(token):
    """The merchant a live access token is for, or None."""
    if token in TEST_TOKENS:
        return TEST_TOKENS[token]
    held = state["access"].get(token)
    if held and held["expires"] > time.time():
        return held["merchant"]
    return None


def find_tender(merchant, label):
    for tender in state["tenders"][merchant]:
        if (tender.get("label") or "").strip().lower() == label.strip().lower():
            return tender
    return None


def tender_ref(tender, expanded):
    if expanded:
        return {"id": tender["id"], "label": tender["label"], "labelKey": tender["labelKey"]}
    return {"id": tender["id"]}


def order_view(order, expand):
    view = {k: v for k, v in order.items() if k not in ("lineItems", "tax")}
    if "lineItems" in expand:
        items = []
        for line in order["lineItems"]:
            item = {k: v for k, v in line.items() if k != "taxRates"}
            if "lineItems.taxRates" in expand:
                item["taxRates"] = {"elements": line["taxRates"]}
            items.append(item)
        view["lineItems"] = {"elements": items[:NESTED_MAX]}
    return view


def payment_view(merchant, payment, expand):
    view = {k: v for k, v in payment.items() if k not in ("tender", "order", "refunds")}
    view["tender"] = tender_ref(payment["tender"], "tender" in expand)
    order = state["orders"][merchant][payment["order"]["id"]]
    view["order"] = order_view(order, set()) if "order" in expand else {"id": order["id"]}
    if "refunds" in expand:
        view["refunds"] = {"elements": payment["refunds"][:NESTED_MAX]}
    return view


def matches(element, condition):
    """Whether a payment or refund meets one Clover filter, such as `createdTime>=1761600000000`."""
    for op in (">=", "<=", "!=", ">", "<", "="):
        if op in condition:
            field, value = condition.split(op, 1)
            break
    else:
        return True
    field = field.strip()
    actual = {"tender.id": element.get("tender", {}).get("id"), "createdTime": element.get("createdTime"),
              "modifiedTime": element.get("modifiedTime"), "result": element.get("result"), "id": element.get("id"),
              "payment.id": element.get("payment", {}).get("id")}.get(field)
    if actual is None:
        return False
    if isinstance(actual, int):
        value = int(value)
    return {">=": actual >= value, "<=": actual <= value, "!=": actual != value, ">": actual > value,
            "<": actual < value, "=": actual == value}[op]


def send_webhook(merchant, object_id, kind, app_id=None, wait=False):
    """Sends one update to the Bell, as Clover batches them, with X-Clover-Auth; the Bell's status, or None."""
    url, code = os.environ.get("FAKE_CLOVER_WEBHOOK_URL"), os.environ.get("FAKE_CLOVER_AUTH_CODE")
    if not (url and code):
        return None
    body = json.dumps({"appId": app_id or APP_ID,
                       "merchants": {merchant: [{"objectId": object_id, "type": kind, "ts": ms()}]}}).encode()
    return post(url, body, {"X-Clover-Auth": code}, "webhook %s %s" % (object_id.split(":")[0] + ":", kind), wait)


def post(url, body, headers, what, wait):
    request = urllib.request.Request(url, data=body, method="POST",
                                     headers={"Content-Type": "application/json", **headers})
    answered = {}

    def deliver():
        try:
            with urllib.request.urlopen(request, timeout=10) as answer:
                answered["status"] = answer.status
        except urllib.error.HTTPError as error:
            answered["status"] = error.code
        except Exception as error:
            log("%s failed: %s" % (what, error))
            return
        log("%s -> %d" % (what, answered["status"]))

    if not wait:
        threading.Thread(target=deliver, daemon=True).start()
        return None
    deliver()
    return answered.get("status")


class Clover(BaseHTTPRequestHandler):
    def send(self, status, body=None, headers=None):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def message(self, status, text):
        self.send(status, {"message": text})

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            return json.loads(self.rfile.read(n) or b"{}")
        except ValueError:
            return None

    def route(self):
        url = urllib.parse.urlparse(self.path)
        return url.path, urllib.parse.parse_qs(url.query)

    def needs_agent(self, path):
        """Clover requires a User-Agent on every API call: refused without one, as a reminder."""
        if (path.startswith("/v3/") or path.startswith("/oauth/")) and not self.headers.get("User-Agent"):
            self.message(400, "User-Agent header is required")
            return True
        return False

    def merchant_of_path(self, path):
        """The merchant a /v3/merchants/{mId}/… path is for, if the bearer token is that merchant's and live."""
        parts = path.split("/")
        merchant = parts[3] if len(parts) > 3 else None
        auth = self.headers.get("Authorization") or ""
        token = auth[len("Bearer "):] if auth.startswith("Bearer ") else None
        if merchant not in MERCHANTS or token is None or token_merchant(token) != merchant:
            self.message(401, "401 Unauthorized")
            return None, parts
        if state["limit"]["count"] > 0:
            state["limit"]["count"] -= 1
            self.send(429, {"message": "429 Too Many Requests"},
                      {"retry-after": str(state["limit"]["retry_after"])})
            return None, parts
        return merchant, parts

    def do_GET(self):
        path, q = self.route()
        if self.needs_agent(path):
            return
        with lock:
            if path == "/oauth/v2/authorize":
                return self.authorize(q)
            if path.startswith("/v3/merchants/"):
                return self.get_v3(path, q)
        self.message(404, "404 Not Found")

    def authorize(self, q):
        """The merchant approves at once: back to redirect_uri with a code for the challenge."""
        one = {k: v[0] for k, v in q.items()}
        if not one.get("client_id") or not one.get("redirect_uri"):
            return self.message(400, "client_id and redirect_uri are required")
        if not one.get("code_challenge"):
            return self.message(400, "code_challenge is required for a low-trust app")
        merchant = one.get("merchant_id", MERCHANT_ID)
        if merchant not in MERCHANTS:
            return self.message(400, "no such merchant")
        code = "clv-c-" + secrets.token_hex(8)
        state["codes"][code] = {"challenge": one["code_challenge"], "client_id": one["client_id"], "merchant": merchant}
        back = {"merchant_id": merchant, "client_id": one["client_id"], "employee_id": EMPLOYEE_ID, "code": code}
        if "state" in one:
            back["state"] = one["state"]
        redirect = one["redirect_uri"]
        redirect += ("&" if "?" in redirect else "?") + urllib.parse.urlencode(back)
        log("authorize: client %s approved for %s" % (one["client_id"], merchant))
        self.send(302, None, {"Location": redirect})

    def get_v3(self, path, q):
        merchant, parts = self.merchant_of_path(path)
        if merchant is None:
            return
        rest = parts[4:]
        expand = {e for v in q.get("expand", []) for e in v.split(",") if e}
        data = MERCHANTS[merchant]
        if not rest:
            answer = {"href": "https://fake-clover/v3/merchants/" + merchant, "id": merchant,
                      "name": data["name"], "owner": data["owner"]}
            if "address" in expand:
                answer["address"] = {"address1": "1 Market St", "city": "San Francisco", "state": "CA",
                                     "zip": "94105", "country": "US"}
            return self.send(200, answer)
        if rest == ["properties"]:
            return self.send(200, {"defaultCurrency": data["currency"], "timezone": data["timezone"],
                                   "tipsEnabled": True, "locale": "en_US", "orderTitle": "AUTO"})
        if rest == ["tenders"]:
            return self.send(200, {"elements": state["tenders"][merchant]})
        if rest == ["payments"]:
            found = [p for p in state["payments"][merchant] if all(matches(p, f) for f in q.get("filter", []))]
            order_by = (q.get("orderBy") or ["createdTime DESC"])[0]
            found.sort(key=lambda p: (p["createdTime"], p["id"]), reverse=not order_by.endswith("ASC"))
            limit = min(int((q.get("limit") or ["100"])[0]), 1000)
            offset = int((q.get("offset") or ["0"])[0])
            return self.send(200, {"elements": [payment_view(merchant, p, expand) for p in found[offset:offset + limit]]})
        if len(rest) == 2 and rest[0] == "payments":
            for payment in state["payments"][merchant]:
                if payment["id"] == rest[1]:
                    return self.send(200, payment_view(merchant, payment, expand))
            return self.message(404, "404 Not Found")
        if len(rest) == 2 and rest[0] == "orders":
            order = state["orders"][merchant].get(rest[1])
            return self.send(200, order_view(order, expand)) if order else self.message(404, "404 Not Found")
        if rest == ["refunds"]:
            found = [r for r in state["refunds"][merchant] if all(matches(r, f) for f in q.get("filter", []))]
            return self.send(200, {"elements": found})
        self.message(404, "404 Not Found")

    def do_POST(self):
        path, _ = self.route()
        if self.needs_agent(path):
            return
        body = self.body()
        if body is None:
            return self.message(400, "the body isn't JSON")
        with lock:
            if path == "/oauth/v2/token":
                return self.token(body)
            if path == "/oauth/v2/refresh":
                return self.refresh(body)
            if path.startswith("/v3/merchants/"):
                return self.post_v3(path, body)
            if path.startswith("/fake/"):
                return self.fake(path, body)
        self.message(404, "404 Not Found")

    def token(self, body):
        if "client_secret" in body:
            # A low-trust app has no secret: Clover refuses a body that carries one.
            return self.message(401, "client_secret is not accepted for a PKCE (low-trust) token request")
        held = state["codes"].pop(body.get("code"), None)
        verifier = body.get("code_verifier") or ""
        made = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        if held is None or held["client_id"] != body.get("client_id") or made != held["challenge"]:
            return self.message(401, "invalid code, client_id or code_verifier")
        log("token: issued for %s" % held["merchant"])
        self.send(200, issue_tokens(held["merchant"]))

    def refresh(self, body):
        held = state["refresh"].get(body.get("refresh_token"))
        if not held or not held["live"] or held["expires"] <= time.time() or not body.get("client_id"):
            log("refresh: refused")
            return self.message(401, "invalid refresh_token")
        # Single use: spent as it is used.
        held["live"] = False
        state["refresh_order"][held["merchant"]].remove(body["refresh_token"])
        log("refresh: new tokens for %s" % held["merchant"])
        self.send(200, issue_tokens(held["merchant"]))

    def post_v3(self, path, body):
        merchant, parts = self.merchant_of_path(path)
        if merchant is None:
            return
        rest = parts[4:]
        if rest == ["tenders"]:
            if DENY_TENDER_WRITE:
                return self.message(403, "403 Forbidden")
            if not body.get("label"):
                return self.message(400, "label is required")
            if body.get("labelKey") not in SYSTEM_LABEL_KEYS:
                return self.message(400, "Error creating merchant tender as system tender does not exist")
            tender = {"id": new_id(), "label": body["label"], "labelKey": body["labelKey"],
                      "enabled": body.get("enabled", True), "visible": body.get("visible", True),
                      "opensCashDrawer": body.get("opensCashDrawer", False),
                      "supportsTipping": body.get("supportsTipping", False), "editable": True}
            state["tenders"][merchant].append(tender)
            log("tenders: %s created as %s" % (tender["label"], tender["id"]))
            return self.send(200, tender)
        if len(rest) == 3 and rest[0] == "orders" and rest[2] == "payments":
            order = state["orders"][merchant].get(rest[1])
            tender = next((t for t in state["tenders"][merchant] if t["id"] == (body.get("tender") or {}).get("id")), None)
            if not order or not tender:
                return self.message(404, "404 Not Found")
            payment = self.record(merchant, order, tender, body.get("amount", order["total"]))
            order["paymentState"] = "PAID"
            send_webhook(merchant, "P:" + payment["id"], "CREATE")
            return self.send(200, payment_view(merchant, payment, {"tender"}))
        self.message(404, "404 Not Found")

    def record(self, merchant, order, tender, amount):
        now = ms()
        payment = {"id": new_id(), "order": {"id": order["id"]}, "tender": tender, "amount": amount,
                   "tipAmount": 0, "taxAmount": order["tax"], "result": "SUCCESS", "createdTime": now,
                   "clientCreatedTime": now, "modifiedTime": now, "offline": False,
                   "employee": {"id": EMPLOYEE_ID}, "device": {"id": DEVICE_ID}, "refunds": []}
        state["payments"][merchant].append(payment)
        return payment

    def fake(self, path, body):
        merchant = body.get("merchant_id", MERCHANT_ID)
        if merchant not in MERCHANTS:
            return self.message(400, "no such merchant")
        if path == "/fake/sale":
            return self.sale(merchant, body)
        if path == "/fake/refund":
            for payment in state["payments"][merchant]:
                if payment["id"] == body.get("payment_id"):
                    now = ms()
                    refund = {"id": new_id(), "amount": payment["amount"], "createdTime": now, "voided": False,
                              "payment": {"id": payment["id"]}, "orderRef": {"id": payment["order"]["id"]},
                              "tender": tender_ref(payment["tender"], True)}
                    payment["refunds"].append(refund)
                    payment["modifiedTime"] = now
                    state["refunds"][merchant].append(refund)
                    state["orders"][merchant][payment["order"]["id"]]["paymentState"] = "REFUNDED"
                    send_webhook(merchant, "P:" + payment["id"], "UPDATE")
                    return self.send(200, refund)
            return self.message(404, "no such payment")
        if path == "/fake/verify":
            url = os.environ.get("FAKE_CLOVER_WEBHOOK_URL")
            if not url:
                return self.message(400, "FAKE_CLOVER_WEBHOOK_URL isn't set")
            code = str(uuid.uuid4())
            print(json.dumps({"verificationCode": code}), flush=True)
            status = post(url, json.dumps({"verificationCode": code}).encode(), {}, "verification", True)
            return self.send(200, {"verificationCode": code, "bell_status": status})
        if path == "/fake/webhook":
            if not body.get("objectId") or not body.get("type"):
                return self.message(400, "objectId and type are required")
            status = send_webhook(merchant, body["objectId"], body["type"], body.get("appId"), wait=True)
            return self.send(200, {"bell_status": status})
        if path == "/fake/limit":
            state["limit"] = {"count": int(body.get("count", 1)), "retry_after": int(body.get("retry_after", 1))}
            log("the next %d /v3 requests are limited" % state["limit"]["count"])
            return self.send(200, state["limit"])
        if path == "/fake/expire-tokens":
            for held in state["access"].values():
                held["expires"] = int(time.time()) - 1
            log("every access token has expired")
            return self.send(200, {"expired": len(state["access"])})
        if path == "/fake/revoke":
            for held in list(state["access"].values()) + list(state["refresh"].values()):
                if held["merchant"] == merchant:
                    held["expires"] = 0
                    held["live"] = False
            state["refresh_order"][merchant].clear()
            log("every token of %s is revoked" % merchant)
            return self.send(200, {"revoked": merchant})
        self.message(404, "404 Not Found")

    def sale(self, merchant, body):
        label = body.get("tender", "Monero")
        tender = find_tender(merchant, label)
        if tender is None:
            hint = " (sign in with the till first: it creates the Monero tender)" if label.lower() == "monero" else ""
            return self.message(400, "no tender labelled %s%s" % (label, hint))
        items = body.get("items") or [["Flat white, oat milk", 2, 450], ["Almond croissant", 1, 375],
                                       ["Granola, per 100 g", 2.5, 120]]
        lines, subtotal = [], 0
        tax_rate = {"id": "TCLOVERDEV001", "name": "Sales tax", "rate": int(TAX_RATE * 10_000_000)}
        for name, quantity, cents in items:
            item = {"id": new_id()}
            if float(quantity).is_integer():
                for _ in range(int(quantity)):
                    lines.append({"id": new_id(), "name": name, "price": cents, "note": None,
                                  "item": item, "printed": False, "taxRates": [tax_rate]})
                subtotal += int(quantity) * cents
            else:
                unit_qty = round(float(quantity) * 1000)
                lines.append({"id": new_id(), "name": name, "price": cents, "unitQty": unit_qty, "unitName": "100 g",
                              "note": None, "item": item, "printed": False, "taxRates": [tax_rate]})
                subtotal += round(cents * unit_qty / 1000)
        tax = round(subtotal * TAX_RATE)
        order = {"id": new_id(), "currency": MERCHANTS[merchant]["currency"], "total": subtotal + tax,
                 "title": body.get("title"), "state": "locked", "paymentState": "PAID", "createdTime": ms(),
                 "tax": tax, "lineItems": lines}
        state["orders"][merchant][order["id"]] = order
        payment = self.record(merchant, order, tender, order["total"])
        send_webhook(merchant, "P:" + payment["id"], "CREATE")
        log("sale: %s %d paid with %s" % (payment["id"], payment["amount"], tender["label"]))
        self.send(200, payment_view(merchant, payment, {"tender", "order", "refunds"}))

    def log_message(self, fmt, *args):
        log(fmt % args)


if __name__ == "__main__":
    port = int(os.environ.get("FAKE_CLOVER_PORT", "8740"))
    print(json.dumps({"listening": "http://127.0.0.1:%d" % port, "app_id": APP_ID, "merchant_id": MERCHANT_ID,
                      "test_token": next(t for t, m in TEST_TOKENS.items() if m == MERCHANT_ID),
                      "other_merchant_id": OTHER_MERCHANT_ID,
                      "other_test_token": next(t for t, m in TEST_TOKENS.items() if m == OTHER_MERCHANT_ID)}),
          flush=True)
    ThreadingHTTPServer(("0.0.0.0", port), Clover).serve_forever()

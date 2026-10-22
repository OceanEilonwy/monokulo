#!/usr/bin/env python3
"""A stand-in Square for running Monokular end to end on a developer's machine.

Only what the till uses: PKCE sign-in (which approves at once), locations,
payments, orders and refunds. Ring up a sale with:

    curl -X POST localhost:8738/fake/sale -d '{"items":[["Flat white, oat milk",2,450],["Almond croissant",1,375]]}'

Standard library only. Not Square: shapes follow its API reference closely
enough for the app, and nothing more.
"""
import base64, hashlib, json, os, secrets, sys, time, urllib.parse
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCATION = {"id": "L-DEV", "name": "Front Counter", "business_name": "The Bean Room",
            "country": "US", "currency": "USD", "timezone": "America/Los_Angeles", "status": "ACTIVE"}
TAX_RATE = "8.875"
state = {"codes": {}, "payments": [], "orders": {}, "refunds": [], "receipt": 1041}


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def money(amount):
    return {"amount": amount, "currency": "USD"}


class Square(BaseHTTPRequestHandler):
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

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def authed(self):
        if not (self.headers.get("Authorization") or "").startswith("Bearer EAAA-dev"):
            self.send(401, {"errors": [{"code": "UNAUTHORIZED", "detail": "bad token"}]})
            return False
        return True

    def do_GET(self):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        if url.path == "/oauth2/authorize":
            # The seller approves at once.
            code = "code-" + secrets.token_hex(8)
            state["codes"][code] = q.get("code_challenge")
            back = q["redirect_uri"] + "?" + urllib.parse.urlencode({"code": code, "state": q.get("state", "")})
            return self.send(302, None, {"Location": back})
        if not self.authed():
            return
        if url.path == "/v2/locations":
            return self.send(200, {"locations": [LOCATION]})
        if url.path == "/v2/payments":
            since = q.get("begin_time", "1970-01-01T00:00:00Z")
            found = [p for p in state["payments"] if p["created_at"] >= since and p["location_id"] == q.get("location_id")]
            return self.send(200, {"payments": found})
        if url.path.startswith("/v2/orders/"):
            order = state["orders"].get(url.path.rsplit("/", 1)[1])
            return self.send(200, {"order": order}) if order else self.send(404, {"errors": [{"code": "NOT_FOUND"}]})
        self.send(404, {"errors": [{"code": "NOT_FOUND"}]})

    def do_POST(self):
        url = urllib.parse.urlparse(self.path)
        body = self.body()
        if url.path == "/oauth2/token":
            if body.get("grant_type") == "authorization_code":
                challenge = state["codes"].pop(body.get("code"), None)
                verifier = body.get("code_verifier", "")
                made = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
                if challenge is None or made != challenge:
                    return self.send(401, {"errors": [{"code": "UNAUTHORIZED", "detail": "bad code or verifier"}]})
            expires = datetime.fromtimestamp(time.time() + 30 * 86400, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            return self.send(200, {"access_token": "EAAA-dev-" + secrets.token_hex(8), "token_type": "bearer",
                                   "expires_at": expires, "merchant_id": "M-DEV",
                                   "refresh_token": "r-" + secrets.token_hex(8)})
        if url.path == "/fake/sale":
            state["receipt"] += 1
            order_id = "ORD-" + secrets.token_hex(6)
            lines, subtotal = [], 0
            for name, qty, cents in body.get("items", [["Flat white", 1, 450]]):
                subtotal += qty * cents
                lines.append({"name": name, "quantity": str(qty), "variation_name": "Regular",
                              "base_price_money": money(cents), "gross_sales_money": money(qty * cents)})
            tax = round(subtotal * float(TAX_RATE) / 100)
            total = subtotal + tax
            created = now()
            state["orders"][order_id] = {"id": order_id, "location_id": LOCATION["id"], "created_at": created,
                                         "line_items": lines, "total_money": money(total),
                                         "taxes": [{"name": "Sales tax", "percentage": TAX_RATE, "applied_money": money(tax)}],
                                         "total_tip_money": money(0)}
            payment = {"id": "PAY-" + secrets.token_hex(6), "created_at": created, "status": "COMPLETED",
                       "source_type": "EXTERNAL", "amount_money": money(total), "total_money": money(total),
                       "location_id": LOCATION["id"], "order_id": order_id,
                       "receipt_number": "#%d" % state["receipt"],
                       "external_details": {"type": "OTHER", "source": body.get("tender", "Monero")}}
            state["payments"].append(payment)
            return self.send(200, payment)
        if not self.authed():
            return
        if url.path == "/v2/refunds":
            state["refunds"].append(body)
            return self.send(200, {"refund": {"id": "REF-" + secrets.token_hex(4), "status": "PENDING", **body}})
        self.send(404, {"errors": [{"code": "NOT_FOUND"}]})

    def log_message(self, fmt, *args):
        sys.stderr.write("square: " + fmt % args + "\n")


if __name__ == "__main__":
    port = int(os.environ.get("FAKE_SQUARE_PORT", "8738"))
    print(json.dumps({"listening": "http://127.0.0.1:%d" % port}), flush=True)
    ThreadingHTTPServer(("0.0.0.0", port), Square).serve_forever()

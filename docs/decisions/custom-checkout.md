# Custom checkout pages

Merchants whose site doesn't suit the full checkout page get role colours,
three stock layouts (Tiny, Compact, Detailed), and a custom template with CSS.
A custom page needs a verified domain, and each version of it is approved by a
DNS record (or, for an onion shop, a file fetched over Tor).

The approved design is the canvas at
<https://claude.ai/artifact/2w1HRFKqcdUi2F1uidvXbf>. This file records what was
settled after it, and each decision made while building it without asking.

## Settled with the owner

- Custom pages send `frame-ancestors` for the store's verified domains.
- The strict Content-Security-Policy applies to every layout, not only custom
  pages, so there is one code path.
- The stock layouts are real templates, passing the same checks as a merchant's.
- The JS client gets a `view` option, with a default frame height per view.
- The share page always shows Detailed, and never a custom page.
- A shop plugin connecting from a second domain is told so and offered a new
  store, rather than being silently skipped.
- No links in templates in the first version.
- While payment is awaited, every page must show status, code, address and the
  XMR amount, plus the fiat amount when the order has one. `{{address short}}`
  counts as the address, which is how Tiny meets the rule.
- Tiny replaces the code once a payment is seen or paid, and has a Copy button
  that gives the full address.
- Publishing is a "Use this template" button, never automatic.
- One short problem line under the status track.
- The wizard's third step is "Approve".
- `/admin/custom-pages` lets an operator turn off a store's custom page. It
  overrides approval and blocks publishing, and drafts stay editable.
- One web domain and one onion address per store is a breaking change. Monokulo
  refuses to start while any store has more, checked before migrations so the
  old version still runs. Nothing is deleted automatically.

## Decisions made while building

Newest last. Each says what was chosen and why, so it can be reversed.

1. **A check with no answer is retried hourly, not daily.** Fixing the outage
   bug means a failed lookup no longer starts the failing clock, and without
   this a brief outage would also delay the next real check by a day.
   `last_error` on a verified, non-failing row marks it, so no new column.
2. **`domain-proof` keeps the proof's timings and `MIN_CHECK_GAP_SECS`;
   `MAX_DOMAINS_PER_STORE` and `new_token` stay in `monokulo`.** How often a
   proof is checked belongs with the proof. How many domains a store may have,
   and how a row's token is made, are the store's business.
3. **A domain nobody can get an answer about stays verified, with no time
   limit.** "Can't check never counts as missing" is the agreed rule. Its cost:
   a domain whose name servers die for good, or an onion row on an instance
   with Tor turned off, keeps counting, shown only as muted text on its card.
   Accepted, since the token was public anyway, so whoever controlled that DNS
   could republish it. An answer that lacks the record is still what lapses a
   domain. Likewise a clock already running keeps running through an outage,
   rather than an outage hiding a real warning.
4. **Fetch metadata can't be verified on Tor Browser from this environment.**
   There is no Tor Browser here. So the gate fails safe: without
   `Sec-Fetch-Dest` a page is treated as not framed and gets the fallback
   layout, so the worst case on Tor is a stock page rather than a leak. It
   needs one manual check on real Tor Browser before relying on onion custom
   pages.

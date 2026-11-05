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
5. **A shop on a second domain is told by going to store setup.** A store with
   a web domain elsewhere is no longer offered to it, so the connect flow goes
   to making a new store for that shop, which says which site it's for. No
   separate notice: the setup page already is one.
6. **A change of domain is a second row, not a second table.** `replaces_id`
   on the waiting row, with partial unique indexes: one of each kind in place,
   one change of each kind underway. Every reader already treats a waiting
   domain as not counting, so the old domain keeps working with no new code.
7. **A suggested site never vanishes.** A site its web domain doesn't cover
   (a shop adopting a store one `www` away) waits as a change of that domain,
   which keeps counting until the new one is verified. A waiting suggestion
   does occupy the store's web place: Remove or Change makes room.
8. **An onion shop may connect to a site-less store with a web domain.** It has
   its own place beside that domain.
9. **The strict policy lets in one outside image: the merchant's own icon.**
   Detailed shows a verified site's favicon, never on Tor. Dropping that trust
   signal wasn't what the policy was for, so `img-src` names that one verified
   origin for that page. Merchant CSS still can't use it, as `url()` is refused
   when it's saved.
10. **The typeface rules stay inline, allowed by hash, rather than a stylesheet.**
    They carry the request's base path, which a baked stylesheet can't, and a
    hash allows exactly that text and nothing a template could add.
11. **Pages with no order on them get no strict policy.** Not-found and
    open-from-shop show no merchant content, and must never be given any.
12. **Templates are parsed by our own strict parser, not an HTML5 parser.** The
    subset is small enough to write in full, it refuses what a browser would
    rearrange, and what's written is always from the tree. A browser test holds
    it to that. `cssparser` reads the CSS, since CSS tokenising (escapes above
    all) has to match browsers exactly. `lightningcss` was the alternative:
    an alpha, and far heavier to compile.
13. **Merchant CSS isn't in a layer.** Unlayered rules beat the stock layered
    ones, so it can restyle anything, and `!important` in the protected layer
    still beats it. `@layer` itself is refused, so a page can't reorder them.
14. **Only `[data-part="..."]` can be selected by attribute, and no ids.**
    Anything else would pin internals as an API, and attribute selectors are
    how CSS reads values out of a page.
15. **A template's `lang` and `aria-label` are allowed; `id` and `data-*` aren't.**
    Ids are how live updates find things, so one could break them.
16. **The CSS sanitiser checks its own output.** A security review broke the first
    version three ways: a trailing `\\ ` escaping the `{` after trimming, `{}` in
    a value being a nested rule to a browser, and a leading `~` reaching outside
    the root. All are fixed and tested, and `written_is_safe` now reads what was
    written as a browser would, knowing nothing of how it was written. A mistake
    in the writer becomes a refused save, not a page that gets out.
17. **Merchant CSS can still make its own page misleading.** It can hide the code
    or cover the page from inside its own root, and any type selector
    (`input`, `svg`) or stock keyframe name is theirs to reach. That's the owner's
    "if a merchant abuses it, that's on them", and each version is approved in
    DNS. The protected layer (PR 7) limits accidents, not intent.
18. **The problem line keeps today's fuller wording, not the mockups' shorter
    one.** It carries safety advice worked out over time ("If you already sent
    it, keep this page open"), and shortening it wasn't what the design was
    after. No line for pending, partial, unconfirmed or confirming, where the
    track, amounts and code say it.
19. **`{{merchant}}` joins the feature table.** Stock layouts open with who's
    asking, and a template should be able to too.
20. **A page says which regions it showed in its stream's URL (`r=1001`).** A
    region only replaces itself when its condition flips, so a refund address
    being typed in one survives. A stream can't otherwise tell whether a page
    it reconnects to is out of date.
21. **`live-pay` still answers for pages open across the upgrade.** Requests
    without `parts=2` get today's parts, as `fragments` already does. Both can
    go a release later.
22. **While a payment confirms, the address's label says "Payment seen. Don't
    send it again".** The plain track removed that sentence, and Compact has no
    "XMR received" to say it otherwise. A review raised a customer paying twice.
23. **`{{refund}}` can't go inside `{{if}}`.** A region's swap would lose what's
    typed and the script that drives the form.
24. **Status changes are announced from a permanent element.** `#checkout-announce`
    keeps its place with only its text replaced, which is what a screen reader
    needs; the plain track otherwise left no live region in normal states.


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
25. **Tiny's address is `ui::CopyValue` in its middle form.** It's the existing
    shortened copy control: without JavaScript it's selectable and focus shows
    it whole, which replaces the extra line the mockup caption proposed.
26. **The Layout setting uses radio cards, with words rather than the mockup's
    thumbnails.** Thumbnails are decoration a real preview page (PR 8) does better.
27. **A page's stream is told its layout (`layout=`).** A store can change its
    choice while a page is open, and the two mustn't disagree.
28. **Stylesheets are served by hash alone.** The URL names no store, so a page's
    source reveals nothing new, and looking one up is one indexed read. A replaced
    one stays a week for pages still open.
29. **Colours are typed as hex.** `<input type=color>` can't be empty, and empty
    means the theme's own. It works without JavaScript, as everything here must.
30. **Order status colours aren't a merchant's, and their words use the merchant's
    text colour.** A review found the track's labels at 1.4:1 on a merchant's dark
    background. The dots keep the status colours, which the merchant can't change.
31. **Stylesheets are written again at every start.** A page's link comes from its
    colours, so a release changing what they're written as would otherwise break
    every styled store until its merchant saved something.
32. **A preview's frame refuses to open on its own.** It needs `Sec-Fetch-Dest:
    iframe` and `Sec-Fetch-Site: same-origin`, so a draft is never a merchant's
    unapproved words at this Monokulo's address, and a missing header counts as
    no. Its CSP also sandboxes it, runs nothing and blocks forms.
33. **A preview's sandbox costs it the page's typeface.** A sandboxed page has no
    origin, so fonts need CORS, and they aren't worth loosening for this. It shows
    in the device font, which is also what a custom page can choose.
34. **The wizard's first two steps merge with no way in from the settings page.**
    Step 3 and the Custom page card come in the next PR, so it's never a dead end.
35. **The store's Layout is the custom page's fallback.** The design had Custom in
    the Layout list plus a separate "if it can't be shown" choice. One setting says
    what shows where the custom page doesn't, with a card for the page itself:
    a page in use is on, "Stop using" is off.
36. **Publishing looks up the proof itself.** Finding it on the step before isn't
    what authorises the change; publishing is. An attacker with a session can't
    skip the page that checks.
37. **`Sec-Fetch-Dest` or `Sec-Fetch-Site` missing means not framed.** That's the
    opposite of `must_open_from_shop`, because here the safe answer to not knowing
    is the store's ordinary layout. If Tor Browser turns out not to send them,
    onion shops get their layout rather than their page, which needs one manual
    check.
38. **A turned-off page keeps its row.** Stop using is refused while it's off, as
    deleting it would be a way back.
39. **An open page on an old version reloads.** Its stream checks the version on
    every change, and says `reload` if the page was replaced or turned off.
40. **Replaced by decision 49.** A page shows only on the site that approved it. A security review found
    that a stolen dashboard could add its own web domain to a store with an onion
    address, approve a page there, and have it show on the shop's onion address,
    whose owner never saw it. So the frame policy lists only the approver, and a
    store with both shows its layout on Tor, which its card says. This reverses the
    plan's "a store with both never needs the file". Letting an onion address
    approve as well is possible later; it isn't needed to be safe.
41. **A site that can't frame the page gets the layout, not a refused frame.**
    Otherwise an unrestricted store embedding on one unverified site would stop its
    customers paying there. Its `Referer` decides. With none, only an onion approver
    gets the page, since Tor Browser hides it for onion shops; anything else asking
    for no `Referer` gets the layout, which is safe.
42. **A frame's own navigations carry `framed=true`.** A refresh, a form or a new
    version is same-origin to the browser, as Monokulo's own share page is on first
    load, so a customer mid-payment would flip to the layout. Only a custom page
    writes the mark, and it counts only with `Sec-Fetch-Dest: iframe` and
    `same-origin`. The page's frame policy still decides who may hold that frame.
43. **"Can't check" still never lapses a custom page.** The review suggested expiring
    a page whose proof hasn't been seen for two weeks, so that a dead name server
    can't keep one showing. That's the agreed rule for domains (settled answer 3)
    with the same accepted cost, and the page's token is public, so whoever held that
    DNS could republish it anyway. Instead, the card now shows why the last look got
    no answer. A page nobody views isn't rechecked until someone does: its first
    view starts any grace period. Accepted, as a page with no views harms nobody.
44. **Live pages follow today's checks.** A page's template and CSS go through the
    same checks as saving when first read, but for the size budget, which is about
    cost and may tighten, and don't show if they fail. Each start
    writes live pages' CSS again from what the domain approved, so a sanitiser fix
    reaches them, and publishing a draft does the same. The approval stands, since
    its hash covers what the merchant wrote and not what Monokulo writes from it.
45. **The API saves a draft with `POST`, not the plan's `PUT`.** The API's macros
    have no `PUT`, and adding one for this route isn't worth it. It's a form like
    the rest. The plan's two approval routes became one, as a draft's body already
    says what to add: `/checkout-page/draft/check`. It also gained the two things
    the dashboard does that the plan left out, discarding a draft and stopping the
    page. Only the secret key has any of it.
46. **The API is held to the dashboard's rule: no verified domain, no draft.** A
    review found its draft's preview would otherwise put anyone's words at this
    Monokulo's address, with no proof of anything. It answers `409`.
47. **Saving a custom page has a larger body limit than the rest of the API.** The
    API's 32 KiB would refuse what the dashboard saves. Its route allows 128 KiB,
    and its template and CSS limits are what actually bound the work.
48. **"The key can't change what customers see" became "can't put new words in front
    of them".** A secret key can still stop using a page, and can republish an older
    version whose record is still on the domain. Both only show words the domain
    approved or the store's layout, so the guide says to remove old records rather
    than the design tracking versions.
49. **A version is approved on every domain a store has.** The owner's call, over
    decision 40 and over tracking each domain apart, which was too complicated. A
    store with a web domain and an onion address needs the record and the file. The
    page has one status, and shows on both or neither. So a stolen dashboard adding a
    domain can't approve a version alone, which is what decision 40 was for, and the
    page shows on Tor. Its costs, accepted: either proof missing fails the page on
    both sites, and with Tor off a store with an onion address can't publish. A live
    page keeps showing through that outage, unless a clock was already running, which
    no answer doesn't stop.
50. **A page shows only on the domains its proof was last found on.** A review found
    a thief could remove the shop's domain, publish on their own, and have those words
    reach the real site once the shop's domain counted again. So each page keeps the
    domains it was found on (migration 0060), and any other that counts shows the
    layout, and is looked at within the hour.
51. **Over each network, a page names only the site that network reaches.** The frame
    policy would otherwise tie a store's onion address to its web domain in every
    response, and so would an identical proof, so the onion's is hashed with its name.
    It also settles a site sending no `Referer`: on the web that's never an onion shop,
    so it gets the layout, not a frame its browser refuses. An onion shop framing
    Monokulo's web address gets the layout, which the embed library never does.

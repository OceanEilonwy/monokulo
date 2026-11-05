# Custom checkout pages

Write your checkout's page yourself: your words, your layout and your CSS, in
the frame on your own site. Each version is approved on your domain, so someone
who steals your dashboard login or secret key can't put new words in front of
your customers.

Most stores don't need this. A store's **Checkout** tab has three layouts,
Detailed, Compact and Tiny, plus its colours and corners. Write a page when
those aren't enough.

## Where it shows

A custom page shows only in a frame of your own sites, once each has approved it.
Everywhere else customers get the layout you chose, so nothing here can stop anyone paying.

| Where | What shows |
| --- | --- |
| A frame on an `https://` page of your web domain, or a subdomain | Your custom page |
| A frame on your onion address | Your custom page |
| A frame on any other site | Your layout |
| A payment link, or the checkout opened by itself | Your layout |
| An embed that asks for one with `view` | That layout |

The page needs your browser's frame request headers to know where it is, and
your site's `Referer` to know whose frame it is. The embed library sends the
`Referer` itself. If you write the `iframe` yourself and your site sets
`Referrer-Policy: no-referrer` or `same-origin`, add
`referrerpolicy="strict-origin"` to it, or customers there will get your layout.

Passing `view` asks for a layout, so leave it out. Set the frame's size with
`width` and `height` in `mount`'s options, as it's 420 by 900 otherwise:

```js Sizing the frame
Monokulo.mount("#pay", order, { width: "560px", height: "720px" });
```

## Write a template

Start on the store's **Checkout** tab, under **Custom page**. You need a
verified domain first, added on its **Domains** tab. A template is a small part
of HTML with `{{…}}` features where the payment details go:

```html A template
<h1 class="welcome">Thanks for choosing {{store}}</h1>
{{status}}
{{if awaiting}}
  <div class="mk-row mk-gap-md">
    {{qr medium}}
    <div class="mk-stack">
      <p>{{amount}} {{fiat}}</p>
      {{address}}
      {{countdown}}
    </div>
  </div>
{{end}}
{{if status=paid|overpaid}}<p class="done">All done. Thank you!</p>{{end}}
{{refund}}
```

**Check and preview** lists what it finds, by line, and fixing a template's
syntax can reveal more. Then it shows the page in every state an order can be in. You can send the preview's
link to someone: it says that it's a preview, and it runs no scripts. A draft
lasts a week from its last save.

### Features

| Feature | What it shows | Options |
| --- | --- | --- |
| `{{store}}` | The store's name | |
| `{{amount}}` | How much to pay, in XMR | `bare`: the number alone |
| `{{fiat}}` | The same in the order's currency, when it has one | |
| `{{order-id}}` | The order's reference | |
| `{{qr}}` | The code to scan, which opens a wallet | `small`, `medium`, `large` |
| `{{address}}` | The address, with Copy | `short`: its middle hidden |
| `{{status}}` | Where the order stands | `icon`: an icon alone |
| `{{countdown}}` | The time left to pay | |
| `{{progress}}` | The payment's confirmations | |
| `{{refund}}` | The customer's refund address | |
| `{{payments}}` | The payments that have arrived | |
| `{{merchant}}` | A header: the store's name and icon, its verified site, and the order's description | |
| `{{message}}` | A few words on what's happening now | |

Only `{{store}}`, `{{amount}}`, `{{fiat}}` and `{{order-id}}` can go inside a
`<p>`, a heading or an inline tag, since a browser would move anything else out
of it. A feature that updates live, which is all but `{{store}}`, `{{order-id}}`
and `{{merchant}}`, can show only once in any one state.

Every page has to show `{{status}}`, always. While an order is waiting for
payment it has to show `{{qr}}`, `{{amount}}` and `{{address}}`, plus `{{fiat}}`
on an order priced in another currency. Without these a customer can't tell
what to pay, or check it against what they agreed to.

### Conditions

`{{if …}}` shows its contents only in some states, and `{{end}}` closes it, in the
same element it opened in. They don't nest, and there's no `{{else}}`. Commas mean
all of them must hold, and `!` before a name (not `status=`) means not.

| Condition | Holds when |
| --- | --- |
| `awaiting` | More can still be paid |
| `final` | The order has finished |
| `double_spend` | A payment was reversed |
| `fiat` | The order is priced in another currency |
| `refund_locked` | The refund address can no longer be changed |
| `status=paid\|overpaid` | The status is one of these |

The statuses are the ones in [Order statuses](../webpages/#order-statuses).
`{{refund}}` can't go inside an `{{if}}`, because it keeps its own state while a
customer types in it.

### What HTML can do

Tags are limited to text and structure: `article`, `aside`, `blockquote`, `div`,
`figure`, `figcaption`, `footer`, `header`, `hr`, `main`, `nav`, `section`, lists
(`ol`, `ul`, `li`), headings, `p`, `span`, `b`, `em`, `i`, `strong`, `small`, `code`
and `br`. The only attributes are `class`, `lang` and `aria-label`.

So a page has no links, images, forms or scripts of its own. It can't take a
customer anywhere else, which is much of why it's safe to show at this address.

## Write its CSS

CSS is optional, up to 32 KiB, and is scoped to your page. A selector can name
elements, your template's classes, these stock classes, and a feature by
`[data-part="qr"]`:

| Class | What it does |
| --- | --- |
| `mk-row`, `mk-stack` | Lays its children out in a row, or a column |
| `mk-center` | Centres its children |
| `mk-card` | A card |
| `mk-gap-sm`, `mk-gap-md`, `mk-gap-lg` | The space between its children |

It can't load anything from another site, whether by `url()`, `@import` or a
font. Rules can't change the `visibility` or `opacity` of the code, the address,
the amount or the status, or the code's background, which guards against
accidents. Other rules can still hide them, so check every state in the preview.
What the page shows customers is yours to answer for.

## Approve it

Each version needs its own proof on each of your domains. Any edit needs a new one. This
is what keeps a stolen login from writing your page.

From the preview, choose **Approve this version**. A web domain approves with a
DNS record. Add this to your domain, with the value the page gives you. On a
verified subdomain such as `shop.example.com`, the host in `example.com`'s zone is
`_monokulo-template.shop`.

| Type | Host | Value |
| --- | --- | --- |
| TXT | `_monokulo-template` | `monokulo-template=…` |

An onion address has no DNS, so your shop serves a small file at
`/.well-known/monokulo-template` instead, with the contents the page gives you. They
differ from the web domain's record, so the two can't be matched to link your sites.
Monokulo fetches it over Tor, and only over Tor. This is weaker than DNS, as
whoever can change your shop's files can approve a page.

A store with both needs both, in the same step. The page then has one status, and if
either proof goes missing, both sites are affected. A domain you add under a page in
use shows your layout until it has that version's proof, and the page starts failing
at the next look, within the hour, so add its proof soon after.

Choose **Check for the record**, **Check for the file** or **Check both**, then
**Use this template**, which appears once every proof is found. Monokulo looks again
as it publishes, and only publishes if they're all there. What's in use keeps showing until then.

Afterwards you can remove the records of older versions, and should: while one is
there, that version can be published again. Keep the `_monokulo` record that
verifies your domain, as a page shows only on domains that count.

## After it's published

Monokulo looks at each proof again about once a day, when customers are viewing
the page. If one is gone, the page keeps showing for 3 days, the card on your
Checkout tab says which, and then customers get your layout on every domain until
it's back. A lookup that gets no answer doesn't count as the record being gone.

A page open when its version changes, or when it stops showing, loads again by
itself. **Stop using** goes back to your layout at any time.

The admin of the Monokulo you're on can turn a page off at any time. The card shows
any reason they gave. Until they turn it back on, you can't publish or stop using
it.

After an upgrade Monokulo checks each page again. If one no longer passes its
checks, customers get your layout, and you can fix it and publish it again.

## From your own server

The store's secret key can do everything here through the API, which suits a
shop that wants to deploy its checkout with its code. The key alone can't put new
words in front of customers: publishing still takes the proof on your domains. It
can stop using a page, which goes back to your layout.

| Do this | With |
| --- | --- |
| Save a draft | `POST /api/v1/checkout-page/draft` |
| Read it, and what approves it | `GET /api/v1/checkout-page/draft` |
| Look for the proof | `POST /api/v1/checkout-page/draft/check` |
| Publish it | `POST /api/v1/checkout-page/draft/publish` |
| Read what's in use | `GET /api/v1/checkout-page` |
| Stop using it | `DELETE /api/v1/checkout-page` |

A draft that's refused answers `422` with up to 20 problems in `errors`, by place
and line. The [API reference](../api/) has each route's answers.

```js Save, approve, publish
const call = async (method, path, body) => {
  const response = await fetch(`https://pay.example/api/v1/checkout-page${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${process.env.MONOKULO_SECRET_KEY}`,
      "Content-Type": "application/json",
    },
    body: body && JSON.stringify(body),
  });
  const answer = await response.json();
  if (!response.ok) throw new Error(answer.detail);
  return answer;
};

const draft = await call("POST", "/draft", { template, css });
// One for each domain: `kind` is "dns", or "file" on an onion address.
for (const approval of draft.approvals) {
  await publishProof(approval); // your DNS provider's API, or your shop's files
}
// DNS takes a while. Up to 12 lookups a minute, publishing included.
while (!(await call("POST", "/draft/check")).found) {
  await new Promise((wait) => setTimeout(wait, 30_000));
}
await call("POST", "/draft/publish");
```

## On Tor

On an onion address, the page shows in a frame of that address, and it needs Tor Browser to send the browser's frame request headers. If it
doesn't, customers there get your layout, which is safe but isn't what you wrote.
Check it once on your own shop in Tor Browser after you publish. While the Monokulo
you're on has Tor off, a new version can't be published on a store with an onion
address, as its file can't be checked. A page in use keeps showing, unless a proof
was already missing when Tor went off: its 3 days keep running.

# Add Monokulo to your site

Your page asks Monokulo for an order, the customer pays in the checkout
Monokulo shows, and your server hears that it's paid before it ships
anything.

## Key steps

1. **Make a store** in Monokulo: *Add a store*. Give it a name and your
   website's address (any page on the site works). Note its
   public key (`pk_…`) on the store's page.
2. **Add the script tag** to your checkout page, from your Monokulo's own
   address:
   `<script src="https://pay.example/static/monokulo-client.js"></script>`.
3. **Create an order** when the customer checks out:
   `Monokulo.createOrder({ publicKey, amount, currency, merchantOrderId })`.
   A server can make the order itself instead, with the store's secret key.
4. **Show the checkout**: `Monokulo.mount("#pay", order, { onPaid })` puts
   Monokulo's payment page in yours.
5. **Handle the paid webhook** on your server: `order.paid`, signed, with
   your own order id in `merchant_order_id`. Ship on this, never on the
   browser's word. See [Webhooks](webhooks/) and
   [When is an order really paid?](paid/)
6. **Verify your domain** with a DNS record, in the store's settings, so
   only your site can show the checkout. Subdomains (`pay.shop.example`)
   are covered too.
7. **Test on stagenet** with free test coins and a stagenet wallet.
8. **Go live**: change the store to your mainnet wallet and make one small
   real payment.

`pay.example` stands for your Monokulo's public address throughout; `pk_…`
for your store's public key.

## Read next

- [Walkthrough: Geomart](walkthrough-geomart/): the steps above, one at a
  time, on a small shop that sells shapes.
- [JS library](js-library/): everything `monokulo-client.js` does.
- [When is an order really paid?](paid/)
- [Webhooks](webhooks/): every field, the signature, retries.

# The WooCommerce plugin

Take Monero in a WooCommerce shop: the plugin connects the shop to a store
in your Monokulo, creates an order there when a customer checks out, and
marks the WooCommerce order paid when Monokulo says it is.

## Install and connect

1. **Install the plugin** in WordPress: *Plugins*, *Add New*, search for
   Monokulo, then *Activate*.
2. **Connect it**: *WooCommerce*, *Settings*, *Payments*, *Monokulo*, then
   *Connect*. Enter your Monokulo's public address when it asks.
3. **Sign in** to Monokulo in the window that opens. It finds your store by
   the shop's address:
   - one of your stores has the shop's site: confirm, and you're done;
   - none has it: set up a store (its name, and the shop's site, filled in
     for you), choose where its money goes, then *Back to WooCommerce*.
   A store you made without a site can take the shop's: add the site in
   the store's settings (the *Store* card), then connect.
4. **Turn Monero on** at checkout: *WooCommerce*, *Settings*, *Payments*.

Any store can do everything at once: take payments at the till, show the
checkout on your own pages, and take WooCommerce orders. There's no kind of
store to choose.

## Orders

The plugin creates each order from your shop's server with the store's
secret key, and names itself (`Monokulo-Client: woocommerce/…`), so the
store's Orders page shows those orders as *WooCommerce*. Orders made with
the secret key by anything else show as *Store API*.

The paid webhook is signed; the plugin checks it before it marks an order
paid. See [When is an order really paid?](../paid/)

## Test first

Take payments into a **stagenet** wallet while you try it, with free test
coins, then change the store to your mainnet wallet in its settings and make
one small real payment.

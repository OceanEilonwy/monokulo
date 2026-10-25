# Introduction

Monokulo is a self-hosted Monero payment gateway. It runs on your own
computer or OpenWrt router, watches the chain with each store's view key,
and tells the store when an order is paid. It never holds a spend key, so
it can never move your coins.

## How the pieces fit

1. **Install Monokulo** on a [computer](../install-computer/) or a
   [router](../install-router/), and point it at a Monero node it can reach.
2. **Make a store** on its admin page, with a wallet's view key.
3. **Take payments**: put the checkout on your site with the
   [JS library](../js-library/), connect a [shop plugin](../plugins/), or
   make orders from your server with the [HTTP API](../api/).
4. **Ship when it's paid**: your server hears `order.paid` from a
   [webhook](../webhooks/).

## Pick a path

- New to Monokulo: [install it on your computer](../install-computer/),
  then follow [Accept your first payment](../guide/).
- Running a shop on WooCommerce or another platform:
  [Shop plugins](../plugins/).
- Writing your own integration: [the API reference](../api/), with a
  recorded request and answer for every route.

## Test first

Every guide works on stagenet, Monero's test network, with free test coins.
Move a store to mainnet only once a test order has gone through end to end.

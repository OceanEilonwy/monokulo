# Decisions: nicer admin screen

Decisions made while implementing `nicer_admin_screen.md`, where the plan
left something open or had to be adapted. Numbered in the order they were
made.

## D1 (step 1): `public_url` goes on General

- **Decision:** monokulo's `public_url` (this instance's public address,
  added after the work pack's setting list was written) is placed on the
  General tab, next to `signup.mode` and the engine connection.
- **Alternatives:** Server (it is about how the instance is reached), or
  Other.
- **Why:** it is one of the first things an operator sets up and plugins
  can't connect until it is set, which is the same "set this first" job the
  General tab has. Server holds process tuning (bind, threads, memory), not
  public identity. Other is meant to stay empty.

## D2 (step 1): the newer exchange-rate providers follow `exchange_rate.*`

- **Decision:** `exchange_rate.coinmarketcap_*` and `exchange_rate.haveno_*`
  (landed on main after the plan was written) go under Payments >
  "Exchange rates" with the rest of `exchange_rate.*`, by prefix.
- **Alternatives:** none worth considering; the plan places the whole
  `exchange_rate.*` family there.
- **Why:** the plan's table says `exchange_rate.*`. The unit test lists
  every key explicitly so a future addition has to be placed on purpose.

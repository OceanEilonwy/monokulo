# Test audit, chunk C (app-rust): progress

Branch `worktree-agent-a1bc4044ad73128f6`, started from origin/main `6bac4c3b1`.
Scope: every Rust crate except the engine. Tests run with
`nice -n 10 cargo nextest run -p <crate>`.

## Before (origin/main 6bac4c3b1, local, 32 cores, warm build)

| Crate | Tests | nextest wall |
|---|---|---|
| monokulo | 1018 (2 skipped) | 21.98 s |
| shared | 176 (4 skipped) | 0.63 s |
| short-id | 9 | 0.20 s |
| ui | 13 | 0.01 s |
| telemetry | 42 | 0.05 s |
| live-settings | 65 | 0.02 s |
| cli-wallet | 64 | 0.14 s |
| mock-woocommerce | 9 | 1.44 s |

## Done

### 1. Additions (N3, and the 1459 / 1653 / 1910 rewrites)
- N3: `http::connect::tests::disconnecting_refuses_and_changes_nothing_while_the_engine_is_unreachable`
  (new). Checked by mutation: dropping the `None` refusal in
  `store_site::disconnect_submit` fails it (the webhook would have been deleted first).
- N3: `http::embed_domains::tests::dismissing_the_any_site_warning_leaves_a_failing_domain_warned_about`
  (new): the dismiss POST, another merchant's 404, and the failing-domain warning still standing
  with nothing to dismiss it. Replaces 1969 (deleted with the views F2 slice).
- N3: 1761 `creating_a_webhook_shows_its_signing_secret_once_and_lists_it_afterward` now reads the
  secret off the creation page and asserts its value is absent on reload.
- 1459 rewritten as `the_checkout_page_shows_the_orders_own_address_and_amount_without_javascript`:
  the order's own address and XMR amount (from the API's create answer), no fiat line on an XMR
  order (from deleted e2e 2499), and the no-JS asserts (deadline in words, no `data-timestamp`,
  `<noscript>` refresh, no site nav), plus the till view leaving out the refund field.
- 1653 rewritten as `every_query_route_reads_for_an_admin_only_and_never_writes`: a table over the
  4 query routes (page, schema, value, export) × {admin read 200, merchant 403, admin write never
  runs, switched off 404}, plus the value route's out-of-range 404.
- 1910 rewritten as a source guard, `views::tests::only_the_checkout_and_the_connection_check_refresh_themselves`:
  only `views/checkout.rs` and `views/challenge.rs` write a meta refresh outside their tests.

## Decisions

## Deviations from the plan

## Tests already failing on main
None in scope: the baseline run above passed everywhere.

## Needs from other chunks

## After

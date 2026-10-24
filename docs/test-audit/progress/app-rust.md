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

### 2. F7 dead code
- `test_whoami` and its `#[cfg(test)]` route are gone. The session tests use `GET /account`:
  1700 is now `a_session_cookie_reaches_a_protected_page_as_the_right_user_and_a_bearer_does_not`
  (the page names the user); 1709 and 1712 are one table, `a_protected_page_refuses_no_session_and_an_unknown_one`;
  1715 folds into 1725 as `logging_in_sets_an_httponly_samesite_session_cookie_that_signs_the_user_in`,
  keeping the `HttpOnly` and `SameSite=Lax` asserts; the logout test and the account tests'
  `signed_in` helper ask `/account` too. −2 tests.
- `XmrIdentityProvider::supported_currencies` removed with its test 2125. −1 test.
- `telemetry::trace::current_trace_id` was already removed upstream (7befca7a7), and 2314 already
  asserts through `trace::traceparent`. Nothing to do.

### 3. F1 views (presentational deletes)
- 125 `views::` tests deleted as the DB lists them (the 126th, 1910, became the source guard in
  slice 1), with the helpers only they used. `views::pos`, `views::home`, `views::connected_apps`
  and `views::domains` have no tests left, so their test modules went. −125 tests.

### 4. F1 http (presentational deletes and rewrites)
- 33 `http::` tests deleted. 1511 kept (the key tool's download and checksum URLs).
- 1573 rewritten as `an_order_without_a_recorded_price_shows_a_dash_never_a_made_up_amount`:
  the Amount row holds a dash and no digit, no meta refresh, and the absolute payment link.
- 1575 rewritten as `the_orders_list_shows_an_order_and_a_fixi_search_gets_only_its_results`:
  keeps the fixi fragment half (`<div id="orders-results">`, no `<html>`, filtered).
- 1600 (now in `api/runtime/tests/orders.rs`) rewritten as
  `the_embed_library_never_calls_the_engine_and_never_talks_to_its_checkout`: only the negative
  asserts (no engine routes, no `postMessage`, checkout.js never touches `window.parent`/`top`) and
  the scripts' content type.
- 1686 and 1692 are in `http/status_page.rs`, which chunk B owns: not deleted here (see Needs).
- `orders::tests::get_page` is now used only by the snp tests, so it is behind `feature = "snp"`.
- −33 tests.

## Decisions

## Deviations from the plan

## Tests already failing on main
None in scope: the baseline run above passed everywhere.

## Needs from other chunks
- Chunk B (`http/status_page.rs`): F1 deletes 1686
  `http::status_page::tests::http_tests::status_page_lists_a_configured_networks_node_and_a_scanner_not_yet_run`
  and 1692 `..._shows_a_plain_error_banner_when_a_remote_engine_is_unreachable_over_http`, and the F2
  merge 1668 `relative_time_never_goes_negative_for_a_clock_that_moved_backward` into 1669
  `relative_time_picks_the_right_bucket`. Also the doc comment at `status_page.rs:1218` names
  `get_status_round_trips_against_a_real_engine`, which G3 deletes here (1263).

## After

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

### 5. F2/F3 views (merges and rewrites)
- Folded into their hosts as rows or steps: checkout states 1872/1873/1886 into 1889 and 1874 into
  the no-JS test; admin 1834 into 1821 and 1853 into 1817; auth 1850/1851 into 1856 (renamed
  `the_login_page_carries_next_only_when_given_and_escaped`); key_entry 1902 into 1906; orders 1909
  into 1915; status 1956/1958/1960 into 1965 (renamed `the_proof_of_work_window_at_each_distance_from_the_tip`)
  and 1957 into 1952; store_settings 1983 into 1985; wallets 2012/2031 into 2029, and 2020 (the
  `Unknown` row)/2021/2033 into 2017 (renamed `retire_waits_for_orders_stores_and_the_check_then_asks_for_the_name`).
- 1969 deleted (its rule is now the dismiss handler test from slice 1).
- Rewrites (research 30): 1875 is `checkout_page_works_without_javascript` (noscript refresh only,
  its off link, the noscript Save, no site nav), for a page with and without a refund address;
  1878 is `the_checkout_escapes_what_the_merchant_typed`; 1868 drops the time-format part;
  1968 is `the_store_page_never_shows_the_engine_endpoint`.
- −19 tests.

### 6. F2/F3 http (tables and trims)
- True tables, one setup: the open-redirect guard (6 into 1463, every input a row, each also
  refused by `SafePath::parse`); piconero formatting (2 into 1487); `derive_payment_error`
  (5 into 1601); POS ownership and auth as `every_pos_route_is_the_owners_alone` (1648, 1649, 1652,
  1664, 1688 into 1658: one row per route, another merchant's page, new order and order status
  each a row); the orders cross-user 404s (1540, 1541, 1583 into 1545: one row per route).
- Steps of the host test, each keeping its own setup (they write state the others would see):
  thresholds 1537/1566/1591/1595 into 1536 (renamed `the_confirmation_settings_refuse_what_they_cannot_hold`),
  1542 into 1546, 1565 into 1568, 1563 into 1585, API order creation 1611/1616/1619 into 1614,
  POS 1645 into 1634 and 1643 into 1644, login 1705 into 1717, webhooks 1774/1776 into 1762
  (renamed `creating_a_webhook_refuses_a_bad_url_or_header_and_registers_nothing`), 1793 into 1778,
  `parse_extra_headers` 1737/1740/1744 into 1736.
- Trims (research 38): 1528 keeps newest-first lines, the engine-lines note, the GET search form
  and its Refresh submit, no meta refresh, the line page with its narrow/widen links, the fixi
  fragment and the retention 404; 1749 keeps newest-first attempts, the signature header, a
  header's name without its value, no secret, and the send-again form.
- −40 tests.

## Decisions
- 1815 (account) and 1826 (admin) stay instead of folding into 1966 (store_settings). The three
  pages keep what was typed through three different view models (`shown`, a field's
  `value`/`saved_value`, the posted `sent` list), and 1826 alone pins "partly saved" against
  "nothing saved". One test across the three would need their private fixtures made crate-wide.

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

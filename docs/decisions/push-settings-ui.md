# Decisions: the push relay's settings and an app's push targets on the pages

Made on 2026-10-28 without the owner to ask, while putting the push relay's
settings on the admin page and an app's push targets on the store's pages.
The approved designs are the source of truth: relay C (the compact card on
the Server tab, opened in place for editing) and targets B (one summary
line per app) with D (the app's page). Each entry says what was chosen,
why, and what else was possible, so any can be reversed knowingly. The
push system's own log is `push.md`.

The code these were read against is `origin/main` at `2b200ef2a`.

## Plan (the principal Rust engineer)

1. **`[relay.*]` joins the admin page as a second `live-settings`
   registry, not as monokulo's own settings.** The relay declares its
   settings once (`push_relay::settings`, two-part keys such as
   `server.listen`), and `settings!` statics can't be declared twice under
   a `relay.` prefix without copying every declaration. So monokulo builds
   a `Registry` over the relay's declarations (`embedded_settings()`) and
   a store that is the options file scoped to `[relay]`
   (`OptionsFile::scoped(RELAY_TABLE)`, the handle `relay::prepare`
   already reads through), resolving against the process environment with
   the `--relay-…` options as its command line (`Env::with_cli(args.relay)`),
   exactly as the embedded engine's registry is built over `[engine.*]`.
   The registry's own `describe()` then gives every field its source chip,
   its padlock and why (a `--relay-…` option, an environment variable, a
   read-only file), and `check`/`save` run `RelayConfig::from_snapshot`'s
   rules between fields (the APNs key needs its ids, an app with
   `apns_topic` needs the key, `unused_ttl_days` within `ttl_days`) before
   anything is written. `live-settings` gains a `FileStore` (an
   `OptionsFile` alone as a `SettingsStore`; the relay has no
   database-kept switch, so `LayeredStore` over a `MemoryStore` would be a
   store that is never written). The page gets a third owner,
   `SettingOwner::Relay`, whose form names are `relay:<key>` (as the
   engine's are `engine:<key>`) and whose fields all land in one card,
   `Group::Relay`, on the Server tab between Monokulo's and the engine's
   server cards. Showing them through the engine's settings-API plumbing
   (decision 16 of `push.md`) was the other way; the relay runs in this
   process, so an HTTP round trip to itself would be theatre.
2. **`relay.enabled` moves into the Push relay card and becomes
   editable.** It stays monokulo's own setting (a value directly in
   `[relay]`, read by monokulo's handle on the file), `Group::of("relay.…",
   Monokulo)` maps it to `Group::Relay`, and `editable: false` goes. A save
   that would leave the relay on without what it needs to start
   (`server.public_uri`, `MONOKULO_RELAY_HANDLE_KEY`) is refused on the
   card, naming `relay.enabled` and what is missing, before anything is
   written: decision 120's reason for the lock (on without a public
   address stops the next start) is kept as a check instead. Monokulo's
   `RelaySwitch` section is kept live (`MonokuloSettings::relay`), so the
   restart reads it rather than the value from start.
3. **Saving applies the relay's settings by restarting its listener
   in-process, from the save handler, after every commit.** One handle,
   `relay::Handle` (on `AppState`), owns the running relay behind a mutex
   with a child `CancellationToken` of the process's stop. The save's
   order is engine, then relay, then monokulo, each checked first and all
   or nothing as today; then, when `relay.enabled` or any `[relay.*]` key
   changed, `Handle::restart()` cancels the old listener (requests in
   flight finish, as at shutdown), reads `Live<RelayConfig>` and
   `Live<RelaySwitch>`, and starts a new relay and listener, or none when
   off. The Reload options file button does the same when the reload
   changed either. The restart is one call from the handler and not a
   `Reloadable::install` on either registry, because the two things that
   decide it live in two registries (one install each would stop and
   start the relay twice on a save that flips the switch and changes a
   key), and because a failed restart at runtime must not stop the
   process, while a bad `[relay.*]` at start must (the start-up path,
   `relay::prepare` and `relay::start`, keeps stopping monokulo; the
   registry it builds is the one the handle keeps).
4. **A restart that fails leaves the relay stopped and says so; it never
   rolls the file back.** The settings were checked and saved; what failed
   is binding the address or opening the store (a port in use, a path
   that can't be made). The card's state becomes `Failed { error, since }`
   (the chip "Not running" with the error in the card's panel), the toast
   says the settings were saved but the relay didn't start, and the next
   save or the next start of monokulo tries again. Putting the old values
   back would hide the operator's change behind a failure they can see
   and fix.
5. **`Relay::start` takes a stop token and its sweep stops with it.** Each
   start spawned `supervise("push relay sweep", …)` for good, so a restart
   would leave the old store's sweep running beside the new one, one more
   per save. `supervise_until` already exists for loops that live while
   something is configured; the standalone binary passes the token it
   already has.
6. **Every `[relay.*]` setting keeps `applies: Restart` in its
   declaration, and the admin page words it for the relay.** The
   declarations are the standalone relay's too, where nothing reloads
   them, so `Restart` is their truth and what `--init` writes. For the
   relay owner the page shows "Applies when the relay restarts, which
   saving does", the save bar's line adds that the relay restarts its
   listener, the toast says what the restart did, and `pending_restart` is
   not shown for relay fields (the restart is the save's). A monokulo
   restart is never asked for by a relay key.
7. **The card's chips come from the handle, nowhere else.** `RelayView`:
   enabled, state (`Off`, `Running { address, since }`, `Failed`), the
   listen address as set and the address actually bound, `public_uri`,
   the app names, whether APNs and FCM are ready (the parsed config has
   the key and the account, or not), and the handles stored, which is a
   count the relay's `HandleStore` gains (`SELECT COUNT(*)` of unexpired
   rows), read only while the relay runs (its database is open only
   then). No "last delivery": the relay stores no push by design, and the
   approved compact card doesn't show one. The panel under the chips
   lists what is missing, one line each, with the fix: the APNs key not
   set (iOS registrations refused until `MONOKULO_RELAY_APNS_KEY` is set
   and monokulo restarts), the FCM account likewise, no `server.public_uri`
   or no handle key while enabled, and the start error when `Failed`. A
   running relay with every platform ready shows no panel.
8. **Secrets stay environment-only and show as set or not set.** The three
   (`handles.id_key`, `apns.key`, `fcm.credentials`) are `sources: [Env]`,
   so the registry locks them with their variable; the page renders a
   relay secret as "Set" or "Not set" with the variable to set, never a
   value or a masked value, and never a Replace or Remove: there is no
   exception for them as there is for the CoinMarketCap key.
9. **`handles.apps` is edited as rows, the node rows' way.** The setting
   is one JSON value (`Json<Apps>`), which no operator should type. The
   card shows one row per app (name, iOS bundle ID, Firebase project ID,
   Remove) and a blank row to add one, as the Monero nodes tab does
   (`admin_nodes::NodeForm`): Remove and Add are plain submit buttons that
   show the tab again with the rows changed and unsaved, so they work
   without JavaScript, and `admin-settings.js` may do the same in the
   page. The handler folds the rows into the setting's JSON before the
   check, and a refused row's error is shown on that row.
10. **Edit settings and Close are links, `?relay=edit` on the Server tab.**
    The compact card is the card; the opened card is the same card with
    its sub-sections shown. A link works without JavaScript; with fixi it
    swaps the card in place like the tab links. A save from the opened
    card comes back opened when it was refused (the typed values are
    there) and closed when it saved.
11. **Without the relay feature the card doesn't exist and `relay.enabled`
    isn't shown.** `relay::Handle::view()` is `None` in the `without`
    module, `tab_groups` leaves `Group::Relay` out when there is no view,
    and `monokulo_fields` drops `relay.enabled` then: a switch that can
    only stop the next start has no place on the page. The setting stays
    declared (it stops a monokulo built without the relay, as today). The
    view types live outside the `cfg` modules so `views::admin` compiles
    both ways; only `crate::relay`'s two modules differ, as they do now.
12. **The app's page is `/stores/{id}/settings/apps/{app_id}`, for the
    store's members, scoped to the store.** `AuthedUser` and
    `load_owned_connection` as every store settings route, then the app is
    read by store and app id together (`Db::app(store_id, app_id)`): an
    app of another store is `404`, as a connect code of another store is.
    Its actions are `POST …/push-targets/{target_id}/remove` and
    `POST …/test-push`, both under the page, both answering `303` back to
    it with what happened in the query (`?removed=1`, `?tested=<push id>`)
    so the page reads the result from the database and shows the toast
    without JavaScript or a flash in memory.
13. **Remove deletes the target by store, app and id in one statement.**
    The API's `delete_push_target` takes the app's key; the dashboard's
    takes the store's member, so the SQL names all three
    (`WHERE id = ? AND app_id = ? AND store_id = ?`): a target can't be
    removed from a page it isn't on. Its unsent pushes go with it
    (cascade), and the log line says the dashboard removed it.
14. **A test push is a push: queued as a `push_deliveries` row and sent by
    the worker's own code, at once, while the merchant waits.** The
    `PushClient` moves from the worker's loop to `Push` (shared, built
    once), so the test uses the same client, the same senders and tokens,
    the same store budget (`push.per_store_per_minute`: "it counts like
    any push"), the same outbound guard (`check_host`, the reach
    `webhooks.allow_private_urls` allows, never an onion address) and the
    same recording (`record_push_attempt`, the target's failure count
    included). A test that fails is retried by the worker like any push.
    At most 6 a minute for a store, as test webhooks (`TESTS_PER_MINUTE`,
    a `RateLimiter<ConnectionId>` on `Push`), refused with the wait on the
    page. The event is the merchant's pick of the seven sale events,
    `sale.paid` by default.
15. **A test push carries no order.** Its payload has the shape of the
    target's kind with `"test": true` and nothing of a sale: an
    `encrypted` target gets the notification shape with the event's name
    and no sale, amount, address or link; a `live-activity` target gets a
    state with the event's phase, `test: true`, `expiresAt` at the push's
    TTL, and no sale label, amount, currency or `paymentURI`. The apps
    guide documents `test`. `push_deliveries.order_id` becomes nullable
    (migration 0054 rebuilds the table, rows copied) rather than holding a
    made-up id, `event_seq` is 0 as `sale.waiting`'s is, and the topic is
    the push's own (`SHA-256(target id ‖ 0x00 ‖ "test:" ‖ event id)`), so a
    test never collapses a sale's push.
16. **Pushes record how long each attempt took.** `push_deliveries` gains
    `last_duration_ms` (migration 0054), set by `record_push_attempt`, so
    the app's page and the test's toast can say "201 in 240 ms" as the
    webhooks' pages do. The worker already measures it for its log line.
17. **The Apps card's summary line is one query per store.**
    `Db::app_push_summaries(store_id)` aggregates `push_targets` with the
    needed `push_deliveries` facts grouped by app in one statement
    (targets, failing targets, the longest run of failures with its last
    status, error and time, the newest delivery), and the card joins it to
    its app rows by id: no query per app. The line reads "N push targets ·
    last push X ago · healthy", or "1 failing (12 in a row, last 502
    14 min ago)", or "No push targets: the app registers one once it's set
    up for pushes", with "Pushes ›" or "Details ›" to the app's page.
    Healthy means every target's run of failures is 0.
18. **Never a target's endpoint or keys on a page: its host alone.**
    `push::Endpoint::host()` is what every page and log line shows
    (decisions 34 and 59 of `push.md`); the page adds "(the push relay)"
    when the host is this instance's relay or the one `relay.monokulo.com`
    the app docs name. `p256dh` and `auth` are never read for a page.
19. **Recent pushes on the app's page are its targets' last 50
    `push_deliveries` rows, newest first, with 7-day counts.** The
    test-only `Db::push_deliveries` becomes production code scoped to an
    app (`push_deliveries_of_app`), its row type with it; a page of all of
    them is not built (the webhooks' deliveries page is the pattern if it
    is ever asked for). Pushes are kept `push.keep_days`, which the page
    says.
20. **Two streams, one feature branch, linear history.** Stream 1 (the
    relay card, its registry and the in-process restart) owns
    `crate::relay`, `push-relay`, `live-settings`, `settings.rs`,
    `main.rs`, `http/admin_settings*`, `views/admin*` and the Server tab's
    browser test. Stream 2 (the Apps card line, the app's page, Remove and
    the test push) owns `db/push.rs`, `db/apps.rs`, `push/*`, the
    migration, `http/apps*`, `views/connected_apps*` and a new app page
    view, and the store settings browser test. Both add routes to
    `http/mod.rs` and styles to `site.css` in their own blocks, and append
    their decisions here from 21 (stream 1) and from 41 (stream 2). The
    shared seam, `relay::Handle` on `AppState`, is on the branch first.

## The relay card and the restart (rust-engineer-1)

Numbered from 21.

## The Apps card, the app's page, Remove and the test push (rust-engineer-2)

Numbered from 41.

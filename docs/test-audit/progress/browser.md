# Chunk B (browser): progress log

Branch `worktree-agent-ab85c284c5dc7a950`, from origin/main `6bac4c3b1`.
Scope: e2e, crates/ui component specs, apps/engine node tests, the coverage
fixture, and monokulo's status-cache setting.

## Done

(Filled in per commit, newest last.)

1. **Status cache setting (Q6 for 2657/2683/2615) and E2E_PREBUILT (T-a).**
   - `engine.status_cache_ms` (settings.rs): default 10 000, `range(1_000, 300_000)`,
     `applies: Restart`, in `ServerConfig` as `status_cache`. The const `CACHE_TTL` in
     http/status_page.rs is gone; the TTL lives in `StatusCacheShared` (an `AtomicU64`,
     default from the setting) and is set with `status_page::set_status_cache_ttl`.
   - Unit test `settings::tests::the_engine_status_is_trusted_for_ten_seconds_unless_set_and_never_under_one`
     pins the default and the 1 s floor.
   - e2e/browser/real-stack.js writes `status_cache_ms = 1000` under `[engine]`; the coverage
     fixture calls `set_status_cache_ttl(.., 1 s)` in code. 2615 (status card follows the engine
     live) went from 14.9 s to 1.5 s.
   - 2045 rewritten: `monokulos_own_calls_to_an_embedded_engine_keep_the_status_dot_green`
     makes one call more than the engine's default instance limit
     (`SERVER_INSTANCE_RATE_LIMIT_PER_MIN.default_value() + 1`, 601) instead of 2 400: 2.0 s to 0.35 s.
   - `every_monokulo_setting_on_the_admin_page_saves_correctly` (admin_settings.rs) saves the new setting too.
   - **E2E_PREBUILT=1** (for chunk A's T-a): real-binaries-setup.js (`buildBinaries`) and
     coverage-fixture.js skip their `cargo build` when it is set. CI then builds
     `real-stack.js`'s `BUILD_ARGS` in a step of its own:
     `cargo build --locked --features monokulo/snp -p engine --bin monokulo-engine -p monokulo --bin monokulo --example coverage_fixture -p engine-test-support --bin fake-monerod`.

## Decisions

## Deviations from the plan

## Tests already failing on main

## Needs from other chunks

- **Chunk C, crates/monokulo/src/main.rs:** apply the status-cache setting at start. After
  `engine: monokulo::http::Engine::new(engine_client),` builds `app_state` (before
  `build_router`), add
  `monokulo::http::status_page::set_status_cache_ttl(&app_state.engine, monokulo_settings.server.load().status_cache);`.
  Until then the real binaries accept `engine.status_cache_ms` (and stop on a bad value) but
  keep the 10 s default, so 2683 doesn't get its speed-up on this branch.
- **Chunk C, crates/monokulo/src/views/admin.rs (test PLACEMENTS table, after the
  `("engine.url", M, General, "engine"),` row near line 2230):** add
  `("engine.status_cache_ms", M, General, "engine"),`.
  `views::admin::tests::the_tab_list_covers_both_registries` fails on this branch without it.

## Timings

# Browser assertion inventory before migration

Task 2.1 of [the coverage breakdown](COVERAGE_WBS.md). Numbers follow source
order in `e2e/pos-playwright/tests/surface.spec.js` at the start of this work.
“Real code” means code shipped by Monokulo that the assertion executes; a
hand-built page in the test is not a product render. Planned replacements must
pass before the corresponding old case is removed. The baseline below predates
browser instrumentation; task 3.2 will add per-file and branch counts before
any removal.

Baseline on 2026-09-26: `cd e2e/pos-playwright && npx playwright test -c
surface.config.js` passed all 24 cases in 29.1 seconds. The three paid stagenet
tests and `pos-fit`'s live-harness cases were not run; they are outside this
offline baseline. The instrumented baseline from tasks 3.1–3.2 passed the 24
surface cases plus two real-rendered fixture cases with two workers. Its source
set and covered branch decisions are saved in
[the baseline paths JSON](coverage-browser-baseline-paths.json). The source tree
was dirty at collection because the original POS work remained uncommitted.

| Authored source | Lines | Branches |
| --- | ---: | ---: |
| `challenge.js` | 43/49 | 15/19 |
| `checkout.js` | 168/201 | 107/160 |
| `monokulo-client.js` | 104/139 | 68/134 |
| `pos-ui/src/main.tsx` | 206/224 | 187/277 |
| **Total** | **521/613** | **377/590** |

| # | Regression claim | Real code exercised | Mocked boundary | Nearest overlap | Decision / replacement |
| --- | --- | --- | --- | --- | --- |
| 1 | QR upload saves an address, then editing clears confirmation | `checkout.js`, `jsQR.js` | Entire checkout HTML and save API | Stagenet POS QR upload tests only saved state | Migrate to real checkout QR and edit test |
| 2 | Server-rendered saved address is editable and restores when cleared | `checkout.js` | Entire checkout HTML and save API | 1, 3; stagenet saved state | Merge into real checkout saved/editable test, retaining restore assertion |
| 3 | Empty field stays neutral; clearing a newly saved field restores it | `checkout.js` | Entire checkout HTML and save API | 2 tests a pre-saved field | Merge into real checkout empty/saved transition test |
| 4 | SSE swaps live status while preserving a focused partial address | `checkout.js` | Entire checkout HTML and SSE stream | Stagenet updates status, not mid-edit | Migrate to real checkout SSE test |
| 5 | Tall checkout has matching background and thick progress bar | CSS extracted from `checkout.rs` | Hand-built checkout DOM | `pos-fit` covers outer fit only | Replace with real checkout geometry test |
| 6 | Address copies, selects fully, and fits at three widths | `checkout.js`, CSS extracted from `checkout.rs` | Hand-built address DOM and clipboard | No direct overlap | Migrate to real checkout copy/selection test |
| 7 | POS header never overlaps checkout iframe at three sizes | `pos-app.css` | Hand-built POS DOM | `pos-fit` checks no scrolling, not overlap | Replace with real POS geometry test |
| 8 | Compact refund buttons sit below full-width input | CSS extracted from `checkout.rs` | Hand-built compact form | No direct overlap | Replace with real compact checkout geometry test |
| 9 | Incomplete address is held; saving state appears before success | `checkout.js` | Entire checkout HTML and gated save API | Rust validates address server-side only | Migrate to real checkout validation/saving test |
| 10 | Server rejection marks input invalid; edit clears and retries | `checkout.js` | Entire checkout HTML and save API | Rust checks rejected address, not browser feedback | Migrate to real checkout rejected/retry test |
| 11 | Network failure shows retryable error without saved state | `checkout.js` | Entire checkout HTML and save API | No stagenet error coverage | Migrate to real checkout failed-save test |
| 12 | Camera rejection leaves upload path available | `checkout.js` | Entire checkout HTML and camera API | No stagenet camera failure | Migrate to real checkout camera-failure test |
| 13 | Empty device list does not hide camera before permission | `checkout.js` | Entire checkout HTML and media devices | 12 covers later failure only | Migrate to real checkout pre-permission test |
| 14 | No-JS refund entry and refresh link remain available | Server markup contract only | Entire checkout HTML | Rust view markup tests, but not browser visibility | Replace with real checkout no-JS test |
| 15 | Health starts at rendered state, then polls healthy/unknown | Real status script extracted from Rust | Entire indicator HTML, clock, status API | No stagenet forced health changes | Replace with real page health-polling test |
| 16 | POS backgrounds, reloads, reopens, cancels, and searches | Shipped `pos-app.js` and CSS | POS shell, all APIs, trivial checkout iframe | Stagenet covers background, not reload/cancel/search | Migrate to real POS page with controlled engine |
| 17 | Stack/list badges and progress icons match all states | Shipped `pos-app.js` and CSS | POS shell and list API | Rust status mapping; stagenet paid/confirming | Migrate to real POS status variants test |
| 18 | Client `refund:false` changes frame URL without stopping SSE | Shipped `monokulo-client.js` | Merchant shell, checkout HTML, SSE | No real stagenet client-library test | Keep protocol claim; use real checkout in frame when fixture exists |
| 19 | Allowed frame loads and blocked frame is refused by CSP | Browser CSP behavior only | CSP header and checkout HTML invented in test | Rust `EmbedPolicy` tests real header | Delete after real Monokulo allowed/blocked frame test |
| 20 | `Sec-Fetch-Dest` distinguishes top page and iframe | Browser request header, synthetic Node server | Frame-only rule invented in test | Rust `must_open_from_shop` tests rule | Delete after real Monokulo frame-only test |
| 21 | JS challenge solves and returns to checkout | Shipped `challenge.js` | Challenge HTML/protocol and final checkout HTML | Rust challenge protocol tests | Migrate to real challenge response and checkout |
| 22 | No-JS challenge waits ten seconds then continues | Browser meta refresh, challenge markup contract | Challenge server and final checkout HTML | Rust challenge markup test | Migrate to real no-JS challenge response |
| 23 | Cross-site iframe challenge continues with and without JS | Shipped `challenge.js`, browser iframe behavior | Shop, challenge server, final checkout HTML | 21–22 lack cross-site frame | Migrate to real frame challenge test |
| 24 | Client retries order creation with a valid challenge proof | Shipped `monokulo-client.js` | Merchant shell and challenge API | Rust proof validation, not client retry | Keep protocol test; use actual Monokulo challenge API when feasible |

## Real stagenet POS tests

| Test | Claim and real code | Boundary / nearest overlap | Decision |
| --- | --- | --- | --- |
| No-JS store launcher | Real store and POS views disable POS without JS | Live harness; surface 14 tests checkout instead | Keep as explicit stagenet wiring check |
| 0-conf trusted payment | Real login, order creation, compact checkout, QR refund, payment, paid status, dashboard order | Live engine/node/wallet; surface 1 and 16 only cover isolated states | Keep outside deterministic default; add optional checkpoints |
| Confirming payment backgrounded | Real payment, confirming state, background stack, eventual completion | Live engine/node/wallet; surface 16 covers reload/cancel/search | Keep outside deterministic default; add optional checkpoints |

## `pos-fit.spec.js` viewport families

| Family | Claim and real code | Boundary / nearest overlap | Decision |
| --- | --- | --- | --- |
| Chromium: seven devices × portrait/landscape | Real POS keypad, filled note, and checkout do not scroll | Live stagenet harness, no payment; surface 7 asserts header/frame overlap | Keep the 14 viewport cases; move to controlled local engine when available |
| WebKit: same 14 sizes when runnable | Same POS fit across Safari layout engine | Live harness; skipped if WebKit unavailable | Keep; record skip status explicitly |
| Resize after load | Real POS remains within viewport after five size changes | Live harness; catches stale keypad measurement | Keep; move to controlled local engine when available |

No test has been deleted in this inventory step.

## Checkout migration (task 2.3)

The first migration removed former surface cases 1–6, 8–14, and 18 after
their claims passed against production checkout markup and scripts in
`coverage-checkout.spec.js`. The real checkout tests now cover QR upload,
saved and empty refund states, delayed saving, server rejection and retry,
network and camera errors, address copy and selection, SSE during an edit,
no-JS manual entry and refresh, compact and tall layout, a paid terminal
state, and `monokulo-client.js` framing the actual checkout with refund
disabled. The deterministic suite passed 22/22 cases after this change.

At this source revision, instrumented coverage after removal is 521/613
lines and 375/590 branches, compared with the 521/613 and 377/590 baseline.
The two stable lost branch decisions are the false payment-address and copy
button guards at checkout.js:10–11. Production checkout always renders both
elements; only the former hand-built page could omit them. Client.js:316's
error fallback was also hit by the former finite fake stream, but the real
fixture keeps its stream open; its conditional and line remain a specific
follow-up for task 2.5. Client.js:105's recursive proof batch varies with a
random challenge token, so its hit status is not a stable test claim.

## POS, challenge, and policy migration (task 2.4)

Production-rendered fixtures now cover the former surface claims 7, 15–17,
and 19–23: POS background/reload/reopen/cancel/search and status badges;
dashboard health polling; challenge continuation with JavaScript, without
JavaScript, and in a cross-site frame; checkout framing restrictions from
Monokulo's actual CSP and frame-only response; and POS/checkout geometry.
The fake-page cases were removed from the committed test source. The
`monokulo-client.js` challenge protocol test remains as a separate boundary.

The local deterministic suite runs 14 controlled Chromium device/orientation
sizes plus five live-resize checks. The independent stagenet `pos-fit` suite
still supplies optional WebKit coverage. The new fit checks found a short
viewport overflow in the current POS CSS; authored `pos.css` and served
`pos-app.css` now constrain the root height and use two columns in short
landscape. All 14 device cases and the resize case passed after the fix.

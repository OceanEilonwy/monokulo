# Browser migration audit

The default browser profile uses a real Monokulo router and a controlled
`scanner-test-support` engine. It does not spend wallet funds or depend on a
public node. The instrumented baseline was captured before removing any
fabricated-page test. The final deterministic profile passed 38 tests on
2026-09-26 with two workers.

| Authored source | Baseline lines | Final lines | Baseline branches | Final branches |
| --- | ---: | ---: | ---: | ---: |
| `challenge.js` | 43/49 | 43/49 | 15/19 | 15/19 |
| `checkout.js` | 168/201 | 170/201 | 107/160 | 108/160 |
| `monokulo-client.js` | 104/139 | 115/139 | 68/134 | 83/134 |
| `pos-ui/src/main.tsx` | 206/224 | 209/224 | 187/277 | 194/277 |
| **Total** | **521/613** | **537/613** | **377/590** | **400/590** |

The final run executed all four authored source areas. The source set and
denominators stayed fixed. Source-location comparison found no lost executable
line. Three baseline branch decisions were not executed after migration:

- `checkout.js:10–11`: guards for missing payment-address and copy controls.
  Production checkout always renders both. The removed hand-written page
  could omit them, so retaining those hits would have tested a non-product
  page shape.
- `monokulo-client.js:316`: EventSource's transient error path while the
  browser still intends to reconnect. The old finite fake stream hit it.
  The real-frame tests now exercise both an active stream and a refused
  stream that falls back to status polling. The remaining reconnect decision
  is a browser transport edge, not a lost merchant-facing state assertion.

The former `surface.spec.js` claims map to final tests as follows:

| Former claims | Final test file and state |
| --- | --- |
| 1–4, 6, 8–14, 18 | `coverage-checkout.spec.js`: QR/save/retry/camera, SSE during edit, copy, no-JS, compact layout, and real client iframe |
| 5 | `coverage-checkout.spec.js`: tall real checkout geometry |
| 7 | `coverage-pos.spec.js`: real header and iframe geometry |
| 15 | `coverage-pos.spec.js`: real dashboard health polling |
| 16–17 | `coverage-pos.spec.js`: background/reload/reopen/cancel/search and API-driven badge variants |
| 19–20 | `coverage-checkout.spec.js`: actual CSP and frame-only response |
| 21–23 | `coverage-challenge.spec.js`: production challenge view and timed JS/no-JS continuation, including cross-site frame |
| 24 | `surface.spec.js`: retained client challenge protocol case |
| `pos-fit` Chromium family | `coverage-fit.spec.js`: 14 controlled sizes and five resizes, each checking keypad, filled, and payment states |

Every named screenshot follows a state assertion. The default profile has
no screenshot-only test. The two initial fixture smoke cases were deleted
when their checkout and POS claims became redundant. A preexisting local,
uncommitted badge test still exists in the working file; the coverage config
excludes its exact title so the measured suite has one badge claim.

Representative one-off mutations were made in authored or served assets and
restored afterward. The QR saved-state test failed when `checkout.js` stopped
marking a saved address; the iPhone SE landscape fit test failed when the POS
root lost its viewport height constraint; and the real client iframe test
failed when `refund:false` stopped changing the frame URL. Logs live under
`target/coverage/mutation-{checkout,pos,embed}.log` for this local run.

The paid stagenet profile is separate: `cargo xtask coverage stagenet` runs
`pos.spec.js` with instrumented checkout/POS assets and writes
`target/coverage/stagenet/` plus `stagenet.json`. It is excluded from
`coverage all` and CI because it broadcasts real stagenet transactions.
That profile has not been run during this deterministic audit; its happy path
cannot replace the error and browser-policy cases above.

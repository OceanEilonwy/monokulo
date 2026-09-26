# POS redesign — implementation task list

The visual contract is [the approved four-screen sketch](docs/pos-background-orders-sketches.html) (and its [static capture](docs/pos-background-orders-sketches.png)). Implement the live POS with **Solid 2.0**, keeping the existing Rust/Axum order service and shared checkout where practical. Work through the numbered tasks in order. Each task is complete only when its stated check passes and the relevant screen matches the sketch at mobile size.

The sketch is illustrative: its amounts, IDs, timestamps, and QR pixels are placeholders. Live data must come from actual orders. **Partial-payment coin option E · Dashed missing piece is the approved design.** The interactive sketch currently defaults to B, so select E when comparing the implementation with it. Use E consistently wherever partial status appears.

## 1. Lock down the design contract and existing behavior

- **Design:** All four screens, the status key, and the five partial-coin options.
- **Implement:** Record the sketch's spacing, typography, colors, card heights, labels, responsive behavior, and transitions as a short visual checklist. Map current POS fields and status values to the sketch. In particular, there is only one optional **Reference** field (`merchant_order_id`), not separate customer and order fields. Identify what belongs in the POS shell versus the existing compact checkout iframe. Treat the sketch's status words as labels; the compact stack itself uses symbols and color without status text.
- **Test:** Review the checklist against all four sketch panels and each status example. Capture the current POS at the target mobile viewport as a baseline for regression comparison.

## 2. Add a Solid 2.0 build and mount point

- **Design:** One responsive POS mini-app inside the existing dashboard page; no new navigation framework is required.
- **Implement:** Add a dedicated Solid 2.0 TypeScript/JSX entry point and Vite build using the Solid 2 compiler plugin. Pin mutually compatible **2.0** release-candidate versions until a stable 2.0 release is available; do not silently fall back to Solid 1. Mount into a Maud-rendered POS root with store ID, public key, currency, and decimal precision passed safely as data. Integrate generated JS/CSS into the Rust asset-serving/build flow so a normal production build works without a development server. Keep the checkout page usable independently.
- **Test:** A clean frontend install/build and normal Rust build serve the compiled POS with correct MIME types; a browser loads it with no console, hydration, or missing-asset errors. Verify the checkout route still works when the POS bundle is absent from that page.

## 3. Define the POS order model and persistence contract

- **Design:** The same order can appear in the foreground, compact stack, and full list without losing its reference, amount, or status.
- **Implement:** Define a typed client model keyed by order ID: reference, amount/currency, XMR amount, creation/expiry time, payment status, confirmations/threshold, error, and whether the merchant backgrounded or cancelled it. Persist POS membership/background state on the server, scoped to the authenticated store connection, rather than only in a browser object. Decide and document how an order created just before a reload is recovered. Provide a bounded, paginated read endpoint for POS orders; keep the store's general order list separate from the POS-specific list. Use the existing order currency metadata and engine order details as sources of truth for financial/status data.
- **Test:** Backend tests prove another store cannot read or change these records, reload/new tab restores a pending backgrounded order, order IDs are deduplicated, and an order created before a lost response can still be found. Pagination works beyond the live stream's current 32-order watch limit.

## 4. Build one reactive order store and data layer

- **Design:** Stack, list, and active payment show identical live state.
- **Implement:** Use Solid 2.0 signals/stores for keypad input, current screen, foreground order ID, order entities, list filter/search, and stream connection state. Keep API and EventSource logic outside visual components. Fetch the POS order list at startup, reconcile it with streamed updates, and preserve the last known payment status through transient disconnects. Do not have UI components manually create or move DOM nodes.
- **Test:** Deterministic data-layer tests cover create → foreground → background → reopen, reload restoration, out-of-order/stale responses, duplicate events, and terminal updates. The rendered stack and list agree after each transition.

## 5. Implement the initial keypad screen

- **Design:** Sketch 1: store breadcrumb/status dot; large amount with currency; 3×4 keypad (`1–9`, `C`, `0`, backspace); one optional **Reference** input; orange **Charge** action. Empty amount disables Charge. No background strip when there are no backgrounded orders.
- **Implement:** Port the existing digit-shift amount behavior, keyboard shortcuts, currency precision (two fiat decimals or twelve for XMR), clear/backspace, max amount limits, and 120-character reference bound into Solid state. Preserve the existing authenticated order-create API and send the reference as `merchant_order_id`. Prevent duplicate submissions and show actionable network/server errors without clearing the entered amount or reference.
- **Test:** Component/browser tests cover touch and keyboard entry, zero/large amounts, both decimal precisions, blank/long reference, duplicate taps, failed creation, and successful creation. Compare a mobile screenshot to sketch 1 for layout, text, and disabled/enabled styles.

## 6. Make order creation recoverable

- **Design:** Charging leads to the awaiting-payment page even if the response is delayed; retrying must not unexpectedly create two payable orders.
- **Implement:** Add an idempotency key or equivalent server-side reconciliation for POS creation. Persist the new order's POS association before reporting success; expose enough information to recover an order after reload or a lost create response. Show clear loading/retry behavior in the keypad.
- **Test:** Repeat the same create request and simulate a response lost after the engine created the order. Exactly one order appears in the POS and can be reopened; a genuinely new charge creates a separate order.

## 7. Implement the payment-awaiting page shell

- **Design:** Sketch 2: reference (or short order ID), order ID, icon-plus-word status badge, expiry notice, exact XMR amount, approximate store-currency amount, payment QR/address and Copy, refund section, **Background order**, **Cancel order**, and the short action hint.
- **Implement:** Render the order heading and actions in Solid. Reuse the existing real compact checkout iframe for payment QR, address, countdown, and refund-address controls, adjusting its compact styling/content so the combined page matches the sketch. Keep real QR generation; never use the sketch's decorative QR. Use a clean interface for status and actions between shell and checkout; avoid duplicated/conflicting status displays and nested scrolling on a phone.
- **Test:** With a real created order, the QR decodes to the displayed payment request/address, Copy copies the real address, amounts and expiry agree with server data, and the iframe fits mobile portrait and short-height devices. Compare screenshot to sketch 2.

## 8. Preserve the refund-address interaction

- **Design:** Sketch 2 includes optional refund address, **Scan refund QR**, **Choose QR image**, and the note that refunds are recorded for the merchant and are not automatic.
- **Implement:** Keep the existing checkout refund form/API, camera scanning, image upload/QR decoding, validation, save feedback, and saved value when an order is backgrounded and reopened. Style the compact checkout controls to the approved layout and wording. Do not move the customer-facing checkout's no-JavaScript behavior into Solid.
- **Test:** Browser tests cover typed address, valid/invalid QR image, camera permission denial, saved-state feedback, background/reopen/reload persistence, and the existing no-JavaScript checkout form. Verify no payment or refund is sent automatically.

## 9. Implement real cancellation semantics

- **Design:** **Cancel order** asks for confirmation; confirmed cancellation appears in the finished list with the grey `×` symbol. Abandoning the dialog leaves the payment open.
- **Implement:** Add an authenticated, store-scoped cancel action with a defined server-side outcome. The current POS `Cancel` button merely resets the keypad, and the engine has no `cancelled` payment status; do not relabel that behavior as cancellation. Define and persist a POS cancellation state, stop presenting its QR as an open checkout, and ensure status handling still surfaces any payment arriving at the already-issued Monero address for merchant review. Reject or carefully resolve cancellation races with detected payment, terminal success, and repeated requests. Keep backgrounding and cancellation as separate actions.
- **Test:** Backend and browser tests cover confirm/decline, pending cancellation, paid/partially paid race, repeat request, unauthorized store, reload, and late incoming payment. A cancelled order is never displayed as a successfully paid sale merely because the merchant dismissed it.

## 10. Allow backgrounding at every appropriate unpaid stage

- **Design:** **Background order** is available immediately on the awaiting-payment page, including **pending** before any funds arrive; it returns to a fresh keypad and leaves the payment open. It remains available while unconfirmed or confirming.
- **Implement:** Remove the current `pending`/confirmation-only gating. Persist background state before navigating to the keypad; keep watching the order. Define how partial/error/terminal states are backgrounded or moved to the finished list, so none disappears automatically after four seconds as the current code does.
- **Test:** Create a pending order, background it before payment, enter and charge a second order, then reopen the first with its same ID, reference, amount, QR, and refund address. Repeat for unconfirmed/confirming; verify persistence across reload and background persistence failure handling.

## 11. Implement the compact, scrollable background stack

- **Design:** Sketch 3: under the top bar, a **Background orders · N** heading with **View all →**, followed by short (about 31px) state-colored cards. Each card contains a status symbol, reference or short ID, and amount; no status word. Width follows content up to a cap, long references ellipsize, and the strip scrolls sideways without a visible scrollbar.
- **Implement:** Render keyed cards from backgrounded orders using a single shared state/icon component. Support touch swipe, trackpad/horizontal wheel, mouse wheel translation where appropriate, keyboard focus/scroll, and an accessible name/title that includes the full reference and status. Avoid stretching cards with short or absent references. Keep the keypad usable below the strip.
- **Test:** Browser tests cover empty/one/many cards, short/long/missing reference, card click to reopen, horizontal overflow at phone widths, keyboard access, and no visible scrollbar in supported browsers. Screenshot-compare to sketch 3.

## 12. Implement the complete status visual language

- **Design:** Use the sketch's colors and symbols consistently in stack, payment badge, and list badge: **pending** partial spinning ring; **unconfirmed** pale disc with no wedge; **confirming** pale disc with a strong progress wedge; **partial** option E, a solid larger isometric Monero coin fragment with a dashed outline of the missing piece; **paid** one full coin; **overpaid** two overlapping full coins; **expired** hourglass with sand at bottom; **double spend** warning `!`; **connection lost** crossed-out Wi-Fi; **cancelled** grey `×` in finished orders. The stack communicates state by symbol/color, while payment and list badges also show text.
- **Implement:** Build reusable inline SVG/CSS symbols based on the sketch, including the small minimalist Monero mark in coin faces. Confirmation wedge: zero confirmations is **unconfirmed** with no wedge; otherwise `min(100%, max(20%, ceil(10 × confirmations / required) × 10%))`, with a defined zero-threshold/paid case. Connection loss overlays the last known status after the existing grace period rather than becoming a payment status. Use reduced-motion treatment for the spinner. Keep status text available to assistive technology.
- **Test:** State-to-visual tests cover every status and edge cases (0/10, 1/10, 2/10, 3/10, 10/10, zero threshold, over-threshold, double-spend priority, disconnect/reconnect). Review rendered icons at stack and badge sizes against the sketch, including color contrast and reduced motion.

## 13. Implement the approved partial-coin artwork

- **Design:** **Option E · Dashed missing piece**: the larger coin fragment stays solid, with its broken edge outlined; a dashed outline shows the missing segment. Match the isometric coin and minimalist Monero mark in the sketch. The other variants are design history, not production options.
- **Implement:** Port option E into one shared SVG/component used by the stack, status key (if retained), and list/payment badges. Preserve its proportions and broken-edge line at both large and compact sizes.
- **Test:** Select E in the sketch and compare the production icon at full and 24px stack size. Verify identical artwork wherever partial status appears and legibility at badge size.

## 14. Implement the full-page background orders list

- **Design:** Sketch 4: back to POS; **Background orders** heading; **Search reference or order ID**; **Active · N** and **Finished · N** tabs; order cards with reference/ID/time, icon-plus-text badge, amount/currency, state detail, and **Open →**. Do not use “Reopen payment”.
- **Implement:** Read from the server-backed POS list, sort predictably by creation time, derive tab counts and search results, and provide loading/empty/error/retry states. Active includes pending, unconfirmed, confirming, and partial; finished includes paid, overpaid, expired, cancelled, and other terminal outcomes as defined by the server. Preserve scroll/search when opening an order and returning. Paginate or load more without overloading the 32-order EventSource limit.
- **Test:** Browser tests cover all filters, counts, reference/ID search, no-reference fallback, long reference, status icons in badges, empty and many-order lists, `Open →` on active and finished items, back navigation, and reload. Screenshot-compare to sketch 4.

## 15. Reopen orders without losing their details

- **Design:** Selecting a stack card or list's **Open →** returns to that order's full payment/detail page; the merchant can go back to a fresh keypad without retyping anything.
- **Implement:** Load the selected order's authoritative details and preserve its POS background membership. Reopen pending orders with the same payable checkout; show settled/expired/cancelled outcomes honestly, without a misleading live payment prompt. Keep refund address and reference attached to the original order. Handle deletion/unknown order and stale tab state gracefully.
- **Test:** Reopen each status from both surfaces, including after reload and from a second browser tab; verify same order ID and financial details, accurate status, and appropriate available actions.

## 16. Make the live stream resilient and bounded

- **Design:** Every visible order updates while the merchant serves another customer; the crossed-out Wi-Fi indicator appears only after a sustained connection loss.
- **Implement:** Retain one EventSource for the bounded set of watched active orders, re-subscribe as orders move in/out, and fetch authoritative snapshots after reconnect. Keep finished history available through paginated reads rather than streaming every past order. Handle stale events, auth expiry, errors, and foreground/background transitions without duplicate listeners. Preserve the sketch's connection-loss overlay and last known payment state.
- **Test:** Simulate payment progressing in a background card, multiple simultaneous orders, stream disconnect shorter/longer than the grace period, reconnect, terminal updates, auth failure, and more than 32 historical orders. Confirm one live connection and no lost/duplicated cards.

## 17. Complete responsive, accessibility, and visual review

- **Design:** Match the approved light-theme sketches across the four mobile states and status examples, with usable controls on real small screens.
- **Implement:** Refine typography (Manrope), spacing, colors, borders, safe areas, touch targets, focus/hover states, long text behavior, dark/theme interactions if supported by existing dashboard chrome, reduced motion, and screen-reader labels. The list and compact stack must remain distinct surfaces; the stack must not grow into the old top-bar disclosure. Document any necessary deviation from the sketch with a screenshot and reason.
- **Test:** Compare browser captures against the reference at its phone size plus narrow, short, and desktop viewports. Check for horizontal page overflow, obscured buttons, nested scroll traps, keyboard-only flow, focus return after dialog/navigation, and accessible status announcements.

## 18. Finish integration checks and retire the old DOM script

- **Design:** The live app follows charge → await → background → stack/list → reopen, including refund and cancellation paths.
- **Implement:** Replace the inline `POS_SCRIPT` and old disclosure styles only after the Solid flow is complete. Update existing Playwright fixtures/tests that currently extract the inline script, retain deterministic browser tests for the redesigned surface, and add backend tests for persistence/cancellation/idempotency. Keep the separate stagenet-funded suite optional; it is not required for routine CI.
- **Test:** Run the frontend build/type checks, relevant Rust tests, and deterministic Playwright flow/visual tests. Perform a final manual visual pass through all four screens and every status symbol against the sketch, then confirm no old POS script or dead CSS remains.

## Implementation references

- Current POS page and inline behavior: [`crates/monokulo/src/views/pos.rs`](crates/monokulo/src/views/pos.rs).
- Current authenticated POS APIs and 32-order stream bound: [`crates/monokulo/src/http/pos.rs`](crates/monokulo/src/http/pos.rs).
- Shared checkout/refund controls: [`crates/monokulo/src/views/checkout.rs`](crates/monokulo/src/views/checkout.rs), [`crates/monokulo/static/checkout.js`](crates/monokulo/static/checkout.js).
- Existing browser coverage: [`e2e/pos-playwright/tests/pos.spec.js`](e2e/pos-playwright/tests/pos.spec.js), [`e2e/pos-playwright/tests/surface.spec.js`](e2e/pos-playwright/tests/surface.spec.js).
- Solid 2.0 RC APIs and build tooling: [official Solid 2.0 announcement](https://github.com/solidjs/solid/discussions/2995), [releases](https://github.com/solidjs/solid/releases), [preview documentation](https://v2.solidjs.com/).

"""Write the synthesised plan's actions (source='final') and print the totals.

Tier 1: research suggestions adopted, with the reviewers' overturns applied.
Tier 2: plan-1's extra cuts, which only plan 1 checked; adopt after the user's yes.
"""
import sqlite3

DB = "/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/target/test-audit/audit.sqlite"
con = sqlite3.connect(DB, timeout=30)
con.execute("DELETE FROM suggestion_tests WHERE suggestion_id IN (SELECT id FROM suggestion WHERE source='final')")
con.execute("DELETE FROM suggestion WHERE source='final'")

# test id -> action the synthesis uses instead of research's (plans 1-4 spot-checks)
OVERRIDE = {
    656: "keep",                 # plan 2: only pin that a deleted store's keys are not re-registered at boot
    1511: "keep",                # plan 2: pins the key tool's download + checksum URLs, 0 ms
    347: "rewrite",              # plan 2: becomes the every-tenant-route auth table (F-A3)
    1600: "rewrite",             # plan 2: negative asserts enforce the embed/embedder boundary rule
    1653: "rewrite",             # plan 2: only test of /admin/query/value; gate table
    1573: "rewrite",             # plans 1,2,4: keep "missing fiat quote shows a dash" money rule
    1575: "rewrite",             # plan 4: only test of the orders search fixi fragment
    1910: "rewrite",             # plan 2: meta-refresh rule as a source guard over views/
    1459: "rewrite",             # plan 1: keep + add "page shows the order's own address and XMR amount"
    1075: "merge-into:1081",     # plan 2: proves the mock receiver really checks signatures
    2499: "merge-into:2496",     # plan 4: what the customer is asked to pay (fiat -> XMR line)
    2502: "merge-into:2497",     # plan 4: only hosted-checkout e2e showing double-spend and expired live
    2623: "rewrite",             # plan 4: gallery feeder; keep health states + capture, drop CSS asserts
    1969: "delete",              # plan 3: pure markup; real rule gets one dismiss-POST handler test
}

def research_links(ids):
    rows = con.execute(
        f"SELECT test_id, action FROM suggestion_tests WHERE suggestion_id IN ({','.join('?'*len(ids))})", ids
    ).fetchall()
    out = {}
    for t, a in rows:
        out[t] = OVERRIDE.get(t, a)
    return out

def add(desc, speedup, cov, links=None):
    cur = con.execute(
        "INSERT INTO suggestion (description, estimated_ci_speedup, estimated_coverage_impact, scope, source) VALUES (?,?,?,?, 'final')",
        (desc, speedup, cov, "all"),
    )
    sid = cur.lastrowid
    for t, a in (links or {}).items():
        con.execute("INSERT OR REPLACE INTO suggestion_tests VALUES (?,?,?)", (sid, t, a))
    return sid

TIER1 = [
    ("F1 [tier 1] Rust presentational deletes: views:: 126 (r28) + http:: 40 (r36), with overturns applied", [28, 36], -0.33),
    ("F2 [tier 1] Rust table-test merges: engine (r17-23), http (r37), views (r29), monokulo units (r33, r34), shared (r11); keep 1715 cookie flags, IDOR rows, 595/220/229/2020 rows", [11, 17, 18, 19, 20, 21, 22, 23, 29, 33, 34, 37], -0.05),
    ("F3 [tier 1] Rust trivial/duplicate deletes and rewrites: engine (r24, 656 kept), shared (r10), live-settings (r14), view/http rewrites (r30, r38)", [10, 14, 24, 30, 38], -0.02),
    ("F4 [tier 1] e2e: delete 62 presentational Chromium tests (r1), fold 40 step tests (r5), merge POS fit tests (r4), 3 rewrites (r6); keep gallery feeders 2684/2692/2658/2609", [1, 4, 5, 6], -0.02),
    ("F5 [tier 1] Firefox to a 3-test smoke (r2); WebKit keeps a wider smoke (POS new/background/resume, camera, counter pay, checkout pays, one fit loop) until the counter-tablet browser is confirmed", [2, 3], 0.0),
    ("F6 [tier 1] Supporting crates and components: xtask judge() + 2 s hanging test (r9), pages folds (r13), mock-woocommerce folds (r12, 1075 merged into 1081), component pixel/CSS specs (r31, r32), cheaper keepers (r16, r35, r39)", [9, 12, 13, 16, 31, 32, 35, 39], 0.0),
    ("F7 [tier 1] Dead code with its tests: test_whoami (5 tests + signed_in helper; repoint to GET /account), XmrIdentityProvider::supported_currencies, telemetry::trace::current_trace_id (r15, r41)", [15, 41], 0.0),
    ("F8 [tier 1] Engine fixed costs: proof chain fixture re-verified on load (r26), every_sql_failure_* split by fault range rather than snapshotted (r25), paused clock with one real-time sibling (r27)", [25, 26, 27], 0.0),
]
NEW = [
    ("N1 [tier 1, add] Real-binaries account-access.spec.js: a merchant signs up, logs out, fails a wrong password, logs back in (r7, r40); plugin connect consent in the same file", 0, 0.0),
    ("N2 [tier 1, add] Every-tenant-route auth table: each tenant route x {no bearer, wrong, own pk_, rotated, disabled} -> 401; replaces 347, covers tenant/key-custody/bundle (no auth test today)", 0, 0.01),
    ("N3 [tier 1, add] disconnect_submit fails closed when the engine is unreachable (no test today); dismiss-POST test for the failing-domain warning (replaces 1969); 1761 asserts the webhook secret is absent on reload", 0, 0.01),
    ("N4 [tier 1, add] Rewrite 2485: the no-JS checkout actually submits the refund Save and sees it saved on reload", 0, 0.0),
]
TIMING = [
    ("T-a [CI] Build the browser binaries in their own step (253 s PR / 384 s release of the browser test step is cargo build)", 253, 0.0),
    ("T-b [CI] Doctests: compile in the Build step; the step runs 3 doctests in 4 s (104-135 s today is recompilation). Keep all 3", 100, 0.0),
    ("T-c [CI] Scale job: separate build step (328 s = 234 s compile + 66 s tests). Touches the excluded scale suite", 234, 0.0),
    ("T-d [CI] Windows Rust tests: nextest archive + --partition count:3 (625 s -> ~210 s), or properties off on Windows (~280-310 s est.); xtask built in the Build step (Summarize spends 284 s compiling it)", 400, 0.0),
    ("T-e [local] xtask test 2471 on sh -c sleep with a 2 s deadline (runs alone for the last 20 s of the Rust run)", 20, 0.0),
    ("T-f [local+CI] real-binaries projects first in coverage-browser.config.js (e2e 62 -> 47 s measured after cuts)", 15, 0.0),
    ("T-g [local+CI] Remove real-clock waits from 8 e2e tests (2592, 2657, 2683, 2526, 2524, 2615, 2507, 2508) via explicit validated tunings, never cfg(test) swaps (e2e 47 -> 30 s measured)", 17, 0.0),
    ("T-h [local] crates/ui Playwright fullyParallel (36 -> 11 s measured)", 25, 0.0),
    ("T-i [local, needs user yes; touches excluded property suite] nextest default profile default-filter leaves out properties and ::scale:: (Rust 35.0 -> 19.2 s measured); ci profile, coverage and engine-properties keep them", 16, 0.0),
    ("T-j [local] cargo xtask test: Rust, then e2e, with components and node alongside; builds before the clock; screenshots/instrumentation only in coverage", 0, 0.0),
    ("T-k [CI] @gallery tag: capture-only tests run only when COVERAGE_SCREENSHOTS=1", 23, 0.0),
]
TIER2 = [
    ("G1 [tier 2, plan 1 only] Delete/fold 23 more kept e2e specs + 23 merges hanging off them (wallet network fold 2640/2641, every-setting-described x3, 15-size POS fit 2552, dropdown spec, no-JS twins kept one per mechanism, duplicates of Rust tests)", [42], -0.02),
    ("G2 [tier 2, plan 1 only] Fold 16 more e2e tests into flows; hidden-tab regression 2524 as an apps/engine node test", [43, 44], 0.0),
    ("G3 [tier 2, plan 1 only] Delete 15 kept Rust/component tests and fold 14 http:: tests into tables", [45, 46], -0.01),
    ("G4 [tier 2, plan 1 only] apps/engine: delete 27 of 35 drawing-math node tests; xtask pages: 13 report tests into 3", [47, 48], 0.0),
]

for desc, ids, cov in TIER1:
    links = research_links(ids)
    sp = sum(con.execute(f"SELECT coalesce(sum(estimated_ci_speedup),0) FROM suggestion WHERE id IN ({','.join('?'*len(ids))})", ids).fetchone())
    add(desc, round(sp, 1), cov, links)
for desc, sp, cov in NEW + TIMING:
    add(desc, sp, cov)
for desc, ids, cov in TIER2:
    links = research_links(ids)
    sp = sum(con.execute(f"SELECT coalesce(sum(estimated_ci_speedup),0) FROM suggestion WHERE id IN ({','.join('?'*len(ids))})", ids).fetchone())
    add(desc, round(sp, 1), cov, links)
con.commit()

def removed(tier):
    q = f"""SELECT count(DISTINCT st.test_id), round(sum(DISTINCT 0),0)
            FROM suggestion s JOIN suggestion_tests st ON st.suggestion_id=s.id
            WHERE s.source='final' AND s.description LIKE ? AND (st.action='delete' OR st.action LIKE 'merge%')"""
    return con.execute(q, (f"%[{tier}%",)).fetchone()[0]

def serial(tier):
    q = f"""SELECT round(sum(t.duration_ms)/1000,1) FROM test t WHERE t.id IN (
              SELECT st.test_id FROM suggestion s JOIN suggestion_tests st ON st.suggestion_id=s.id
              WHERE s.source='final' AND s.description LIKE ? AND (st.action='delete' OR st.action LIKE 'merge%'))"""
    return con.execute(q, (f"%[{tier}%",)).fetchone()[0]

for tier in ("tier 1", "tier 2"):
    print(tier, "tests removed or merged:", removed(tier), "measured serial s of those tests:", serial(tier))
print(con.execute("SELECT count(*) FROM test WHERE test_type='presentational'").fetchone()[0], "presentational tests in DB")

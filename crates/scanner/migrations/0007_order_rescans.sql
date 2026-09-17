-- `docs/order_rescan_wbs.md` Phase 1.2: durable state for a merchant-triggered
-- historical rescan of one order's subaddress, surviving a server restart mid-job
-- (decision 2). `current_height` is the resumable progress cursor - a restart picks
-- a still-`running` row back up from here, not from `from_height` again, and not
-- from `to_height` recomputed against a new tip (`to_height` is fixed once at
-- trigger time, so a job converges even across several restarts).
--
-- `status` deliberately has no `interrupted` value: a row left `running` when the
-- process stopped simply *is* still running, as far as this table is concerned - the
-- engine re-spawns its runner on the next boot (`scanner::run_rescan_job`, fed by
-- `Store::list_running_rescans`) rather than tracking a separate "was interrupted"
-- fact. `failed` is the only terminal-but-unsuccessful state, and never auto-resumes.
CREATE TABLE order_rescans (
    id             TEXT PRIMARY KEY,
    order_id       TEXT NOT NULL REFERENCES orders(id),
    tenant_id      TEXT NOT NULL REFERENCES tenants(id),
    minor_index    INTEGER NOT NULL,
    -- Purely informational (decision 4/WBS 1.2) - what actually governs the walk is
    -- from_height/to_height, already resolved to concrete heights at trigger time.
    mode           TEXT NOT NULL CHECK (mode IN ('simple', 'advanced')),
    status         TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    from_height    INTEGER NOT NULL,
    to_height      INTEGER NOT NULL,
    current_height INTEGER NOT NULL,
    error          TEXT,
    started_at     INTEGER NOT NULL,
    finished_at    INTEGER,
    updated_at     INTEGER NOT NULL
);

-- The one-job-per-tenant guardrail (WBS 1.2 / decision 4): a partial unique index
-- rather than an application-level check-then-insert, so the guarantee holds even
-- under concurrent trigger requests - a second INSERT while one row is still
-- `running` fails the constraint atomically instead of racing a SELECT against it.
CREATE UNIQUE INDEX order_rescans_one_running_per_tenant
    ON order_rescans (tenant_id)
    WHERE status = 'running';

-- Looking up an order's own rescan history (status display, WBS Phase 5's "scan
-- range" UI) is always scoped to one order.
CREATE INDEX order_rescans_order_id ON order_rescans (order_id);

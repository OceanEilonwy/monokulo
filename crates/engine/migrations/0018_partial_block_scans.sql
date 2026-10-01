-- A tick may stop halfway through an oversized block. Only fully checked
-- blocks move the network cursor; these rows retain bounded per-tenant work.
CREATE TABLE partial_block_progress (
    network TEXT NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    height INTEGER NOT NULL,
    block_hash TEXT NOT NULL,
    window_generation TEXT NOT NULL,
    next_tx_index INTEGER NOT NULL,
    PRIMARY KEY (network, tenant_id)
);

-- Matches stay staged until the block hash has been checked again and its
-- cursor commits. An incomplete block must not produce a payment webhook.
CREATE TABLE partial_block_matches (
    network TEXT NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    txid TEXT NOT NULL,
    output_index INTEGER NOT NULL,
    amount_piconero INTEGER NOT NULL,
    key_images_json TEXT NOT NULL,
    seen_at_utc INTEGER NOT NULL,
    PRIMARY KEY (network, tenant_id, order_id, txid, output_index)
);

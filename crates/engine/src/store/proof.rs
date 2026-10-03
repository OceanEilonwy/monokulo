//! Durable state for proof-of-work checking (docs/proof_of_work.md): which
//! networks check, each one's anchor, its proven chain and the RandomX keys
//! below it, and the settlement ceiling those give.

use rusqlite::{params, OptionalExtension};
use shared::network::SqlNetwork;

use super::work::sql_height;
use super::{Result, Store, StoreError};
use crate::pow::{ProvenBlock, SEEDHASH_EPOCH_BLOCKS};

/// A network's checking state, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofNetwork {
    pub enabled_at: i64,
    pub anchor: Option<Anchor>,
}

/// The block a network's proven chain starts from, and how it was trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub height: u64,
    pub hash: String,
    /// How many of `nodes` configured nodes gave it.
    pub agreed: u32,
    pub nodes: u32,
    pub anchored_at: i64,
}

/// An anchor to write: itself, its window of blocks (oldest first, ending
/// at the anchor) and the RandomX keys below the window that the blocks
/// after it need.
pub struct NewAnchor {
    pub agreed: u32,
    pub nodes: u32,
    pub window: Vec<ProvenBlock>,
    pub seeds: Vec<(u64, [u8; 32])>,
}

/// What a write to the proven chain found, when it didn't happen.
#[derive(Debug, PartialEq, Eq)]
pub enum ProvenWrite {
    Written,
    /// The proven chain no longer ends where the blocks were checked from
    /// (it was rewound or anchored again meanwhile): nothing was written.
    Stale,
}

fn hash_hex(id: &[u8; 32]) -> String {
    hex::encode(id)
}

fn read_hash(text: &str) -> rusqlite::Result<[u8; 32]> {
    let bytes = hex::decode(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            "a block hash that isn't 32 bytes".into(),
        )
    })
}

fn row_to_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProvenBlock> {
    let height: i64 = row.get(0)?;
    let hash: String = row.get(1)?;
    let timestamp: i64 = row.get(2)?;
    let cumulative: String = row.get(3)?;
    Ok(ProvenBlock {
        height: u64::try_from(height).unwrap_or(0),
        id: read_hash(&hash)?,
        timestamp: u64::try_from(timestamp).unwrap_or(0),
        cumulative_difficulty: cumulative.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

const BLOCK_COLUMNS: &str = "height, block_hash, timestamp, cumulative_difficulty";

impl Store {
    /// Turns checking on for `network`: from now its orders settle only on
    /// proven blocks. Nothing changes if it is on already.
    pub fn enable_proof(&self, network: monero::Network, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO proof_networks (network, enabled_at) VALUES (?1, ?2)",
            params![SqlNetwork(network), now],
        )?;
        Ok(())
    }

    /// Turns checking off for `network` and forgets its anchor and proven
    /// chain: turned on again, it anchors afresh.
    pub fn disable_proof(&self, network: monero::Network) -> Result<()> {
        self.in_transaction(|s| {
            for table in ["proof_networks", "proven_blocks", "proof_seeds"] {
                s.conn.execute(
                    &format!("DELETE FROM {table} WHERE network = ?1"),
                    [SqlNetwork(network)],
                )?;
            }
            Ok(())
        })
    }

    /// Forgets `network`'s anchor and proven chain, keeping checking on:
    /// the next round anchors afresh (an operator's choice after a reorg
    /// deeper than the anchor).
    pub fn forget_anchor(&self, network: monero::Network) -> Result<()> {
        self.in_transaction(|s| {
            for table in ["proven_blocks", "proof_seeds"] {
                s.conn.execute(
                    &format!("DELETE FROM {table} WHERE network = ?1"),
                    [SqlNetwork(network)],
                )?;
            }
            s.conn.execute(
                "UPDATE proof_networks SET anchor_height = NULL, anchor_hash = NULL,
                    anchor_agreed = NULL, anchor_nodes = NULL, anchored_at = NULL
                 WHERE network = ?1",
                [SqlNetwork(network)],
            )?;
            Ok(())
        })
    }

    /// `network`'s checking state, or `None` while checking is off.
    pub fn proof_network(&self, network: monero::Network) -> Result<Option<ProofNetwork>> {
        self.conn
            .query_row(
                "SELECT enabled_at, anchor_height, anchor_hash, anchor_agreed, anchor_nodes, anchored_at
                 FROM proof_networks WHERE network = ?1",
                [SqlNetwork(network)],
                |row| {
                    let height: Option<i64> = row.get(1)?;
                    let anchor = match height {
                        Some(height) => Some(Anchor {
                            height: u64::try_from(height).unwrap_or(0),
                            hash: row.get(2)?,
                            agreed: row.get(3)?,
                            nodes: row.get(4)?,
                            anchored_at: row.get(5)?,
                        }),
                        None => None,
                    };
                    Ok(ProofNetwork {
                        enabled_at: row.get(0)?,
                        anchor,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Takes `anchor` as `network`'s, replacing any proven chain: its
    /// window becomes the proven chain's first blocks. Refused (nothing
    /// written) while checking is off or the window is empty.
    pub fn write_anchor(
        &self,
        network: monero::Network,
        anchor: &NewAnchor,
        now: i64,
    ) -> Result<()> {
        let Some(top) = anchor.window.last() else {
            return Err(StoreError::NotFound);
        };
        self.in_transaction(|s| {
            let updated = s.conn.execute(
                "UPDATE proof_networks SET anchor_height = ?2, anchor_hash = ?3,
                    anchor_agreed = ?4, anchor_nodes = ?5, anchored_at = ?6
                 WHERE network = ?1",
                params![
                    SqlNetwork(network),
                    sql_height(top.height)?,
                    hash_hex(&top.id),
                    anchor.agreed,
                    anchor.nodes,
                    now
                ],
            )?;
            if updated == 0 {
                return Err(StoreError::NotFound);
            }
            for table in ["proven_blocks", "proof_seeds"] {
                s.conn.execute(
                    &format!("DELETE FROM {table} WHERE network = ?1"),
                    [SqlNetwork(network)],
                )?;
            }
            s.insert_proven(network, &anchor.window, false)?;
            for (height, hash) in &anchor.seeds {
                s.conn.execute(
                    "INSERT OR REPLACE INTO proof_seeds (network, height, block_hash) VALUES (?1, ?2, ?3)",
                    params![SqlNetwork(network), sql_height(*height)?, hash_hex(hash)],
                )?;
            }
            Ok(())
        })
    }

    fn insert_proven(
        &self,
        network: monero::Network,
        blocks: &[ProvenBlock],
        checked: bool,
    ) -> Result<()> {
        let mut insert = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO proven_blocks
                (network, height, block_hash, timestamp, cumulative_difficulty, checked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for block in blocks {
            insert.execute(params![
                SqlNetwork(network),
                sql_height(block.height)?,
                hash_hex(&block.id),
                i64::try_from(block.timestamp).unwrap_or(i64::MAX),
                block.cumulative_difficulty.to_string(),
                checked,
            ])?;
        }
        Ok(())
    }

    /// The proven chain's newest block.
    pub fn proven_tip(&self, network: monero::Network) -> Result<Option<ProvenBlock>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {BLOCK_COLUMNS} FROM proven_blocks WHERE network = ?1
                     ORDER BY height DESC LIMIT 1"
                ),
                [SqlNetwork(network)],
                row_to_block,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The proven chain's oldest block.
    pub fn proven_floor(&self, network: monero::Network) -> Result<Option<ProvenBlock>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {BLOCK_COLUMNS} FROM proven_blocks WHERE network = ?1
                     ORDER BY height ASC LIMIT 1"
                ),
                [SqlNetwork(network)],
                row_to_block,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Up to `count` proven blocks ending at `top`, oldest first.
    pub fn proven_blocks_ending_at(
        &self,
        network: monero::Network,
        top: u64,
        count: u64,
    ) -> Result<Vec<ProvenBlock>> {
        let top = sql_height(top)?;
        let bottom = top.saturating_sub(sql_height(count)?.saturating_sub(1));
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT {BLOCK_COLUMNS} FROM proven_blocks
             WHERE network = ?1 AND height BETWEEN ?2 AND ?3 ORDER BY height"
        ))?;
        let rows = statement
            .query_map(params![SqlNetwork(network), bottom, top], row_to_block)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The proven block at `height`.
    pub fn proven_block(
        &self,
        network: monero::Network,
        height: u64,
    ) -> Result<Option<ProvenBlock>> {
        Ok(self
            .proven_blocks_ending_at(network, height, 1)?
            .into_iter()
            .next())
    }

    /// The RandomX key at `height` (a multiple of 2048): the proven block
    /// there, or the key kept below the proven chain.
    pub fn proof_seed(&self, network: monero::Network, height: u64) -> Result<Option<[u8; 32]>> {
        if let Some(block) = self.proven_block(network, height)? {
            return Ok(Some(block.id));
        }
        let hash: Option<String> = self
            .conn
            .query_row(
                "SELECT block_hash FROM proof_seeds WHERE network = ?1 AND height = ?2",
                params![SqlNetwork(network), sql_height(height)?],
                |row| row.get(0),
            )
            .optional()?;
        hash.map(|hash| read_hash(&hash))
            .transpose()
            .map_err(Into::into)
    }

    /// Adds `blocks` (checked, consecutive, oldest first) to the proven
    /// chain, replacing everything above `parent` with them: an extension
    /// when `parent` is the tip, a switch to a heavier branch when it is
    /// lower. Written only if the proven block at `parent.height` is still
    /// `parent`, in one transaction, so a branch checked against a chain
    /// that changed meanwhile is never mixed into it.
    pub fn write_proven(
        &self,
        network: monero::Network,
        parent: &ProvenBlock,
        blocks: &[ProvenBlock],
    ) -> Result<ProvenWrite> {
        self.in_transaction(|s| {
            if s.proven_block(network, parent.height)?.as_ref() != Some(parent) {
                return Ok(ProvenWrite::Stale);
            }
            s.conn.execute(
                "DELETE FROM proven_blocks WHERE network = ?1 AND height > ?2",
                params![SqlNetwork(network), sql_height(parent.height)?],
            )?;
            s.insert_proven(network, blocks, true)?;
            Ok(ProvenWrite::Written)
        })
    }

    /// Drops proven blocks below `keep_from`, keeping the RandomX keys among
    /// them that blocks from `keep_from` on may still need.
    pub fn prune_proven(&self, network: monero::Network, keep_from: u64) -> Result<()> {
        let oldest_key = crate::pow::seed_height(keep_from);
        self.in_transaction(|s| {
            s.conn.execute(
                "INSERT OR REPLACE INTO proof_seeds (network, height, block_hash)
                 SELECT network, height, block_hash FROM proven_blocks
                 WHERE network = ?1 AND height < ?2 AND height >= ?3 AND height % ?4 = 0",
                params![
                    SqlNetwork(network),
                    sql_height(keep_from)?,
                    sql_height(oldest_key)?,
                    sql_height(SEEDHASH_EPOCH_BLOCKS)?
                ],
            )?;
            s.conn.execute(
                "DELETE FROM proven_blocks WHERE network = ?1 AND height < ?2",
                params![SqlNetwork(network), sql_height(keep_from)?],
            )?;
            s.conn.execute(
                "DELETE FROM proof_seeds WHERE network = ?1 AND height < ?2",
                params![SqlNetwork(network), sql_height(oldest_key)?],
            )?;
            Ok(())
        })
    }

    /// The highest block an order on `network` may newly settle on, while
    /// checking is on: the highest recorded block that is also proven (by
    /// its id, so every block below it is too), or 0 when there is none.
    /// `None` while checking is off.
    pub fn proof_ceiling(&self, network: monero::Network) -> Result<Option<u64>> {
        if self.proof_network(network)?.is_none() {
            return Ok(None);
        }
        let height: Option<i64> = self.conn.query_row(
            "SELECT MAX(p.height) FROM proven_blocks p
             JOIN scanned_blocks s
               ON s.network = p.network AND s.height = p.height AND s.block_hash = p.block_hash
             WHERE p.network = ?1",
            [SqlNetwork(network)],
            |row| row.get(0),
        )?;
        Ok(Some(height.map_or(0, |h| u64::try_from(h).unwrap_or(0))))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const NET: monero::Network = monero::Network::Mainnet;

    fn block(height: u64, tag: u8) -> ProvenBlock {
        let mut id = [tag; 32];
        id[..8].copy_from_slice(&height.to_le_bytes());
        ProvenBlock {
            height,
            id,
            timestamp: 1_000 + height,
            cumulative_difficulty: u128::from(height) << 70,
        }
    }

    fn anchored(store: &Store, from: u64, to: u64) {
        store.enable_proof(NET, 1).unwrap();
        store
            .write_anchor(
                NET,
                &NewAnchor {
                    agreed: 2,
                    nodes: 3,
                    window: (from..=to).map(|h| block(h, 0)).collect(),
                    seeds: vec![(2048, [9; 32])],
                },
                5,
            )
            .unwrap();
    }

    #[test]
    fn checking_holds_settlement_from_the_moment_it_is_on() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.proof_ceiling(NET).unwrap(), None, "off: no ceiling");
        store.enable_proof(NET, 1).unwrap();
        assert_eq!(
            store.proof_ceiling(NET).unwrap(),
            Some(0),
            "on, nothing proven"
        );
        let state = store.proof_network(NET).unwrap().unwrap();
        assert_eq!((state.enabled_at, state.anchor), (1, None));
        store.enable_proof(NET, 7).unwrap();
        assert_eq!(
            store.proof_network(NET).unwrap().unwrap().enabled_at,
            1,
            "kept"
        );
    }

    #[test]
    fn an_anchor_is_stored_with_its_window_and_keys() {
        let store = Store::open_in_memory().unwrap();
        anchored(&store, 3000, 3010);
        let anchor = store.proof_network(NET).unwrap().unwrap().anchor.unwrap();
        assert_eq!(
            (
                anchor.height,
                anchor.hash,
                anchor.agreed,
                anchor.nodes,
                anchor.anchored_at
            ),
            (3010, hex::encode(block(3010, 0).id), 2, 3, 5)
        );
        assert_eq!(store.proven_tip(NET).unwrap(), Some(block(3010, 0)));
        assert_eq!(store.proven_floor(NET).unwrap(), Some(block(3000, 0)));
        assert_eq!(
            store.proven_blocks_ending_at(NET, 3005, 3).unwrap(),
            vec![block(3003, 0), block(3004, 0), block(3005, 0)]
        );
        assert_eq!(store.proof_seed(NET, 2048).unwrap(), Some([9; 32]));
        assert_eq!(store.proof_seed(NET, 4096).unwrap(), None);

        // An anchor with checking off is refused.
        let other = Store::open_in_memory().unwrap();
        assert!(other
            .write_anchor(
                NET,
                &NewAnchor {
                    agreed: 1,
                    nodes: 1,
                    window: vec![block(1, 0)],
                    seeds: vec![]
                },
                1
            )
            .is_err());
        assert_eq!(other.proven_tip(NET).unwrap(), None);
    }

    #[test]
    fn the_proven_chain_extends_switches_and_refuses_a_stale_parent() {
        let store = Store::open_in_memory().unwrap();
        anchored(&store, 100, 110);
        let tip = block(110, 0);
        assert_eq!(
            store
                .write_proven(NET, &tip, &[block(111, 0), block(112, 0)])
                .unwrap(),
            ProvenWrite::Written
        );
        // A heavier branch from 110 replaces 111 and 112.
        assert_eq!(
            store
                .write_proven(NET, &tip, &[block(111, 1), block(112, 1), block(113, 1)])
                .unwrap(),
            ProvenWrite::Written
        );
        assert_eq!(store.proven_tip(NET).unwrap(), Some(block(113, 1)));
        assert_eq!(store.proven_block(NET, 111).unwrap(), Some(block(111, 1)));
        // Checked against the old 112: refused, nothing written.
        assert_eq!(
            store
                .write_proven(NET, &block(112, 0), &[block(113, 0)])
                .unwrap(),
            ProvenWrite::Stale
        );
        assert_eq!(store.proven_tip(NET).unwrap(), Some(block(113, 1)));
    }

    #[test]
    fn pruning_keeps_the_keys_still_needed() {
        let store = Store::open_in_memory().unwrap();
        anchored(&store, 4000, 6200);
        store.prune_proven(NET, 6150).unwrap();
        assert_eq!(store.proven_floor(NET).unwrap(), Some(block(6150, 0)));
        // Blocks from 6150 use the key at 4096 (seed_height(6150) = 4096).
        assert_eq!(
            store.proof_seed(NET, 4096).unwrap(),
            Some(block(4096, 0).id)
        );
        // The one at 2048 is older than any of them needs.
        assert_eq!(store.proof_seed(NET, 2048).unwrap(), None);
        // 6144 is still a proven block.
        assert_eq!(
            store.proof_seed(NET, 6144).unwrap(),
            Some(block(6144, 0).id)
        );
    }

    #[test]
    fn the_ceiling_is_the_highest_recorded_block_that_is_proven() {
        let store = Store::open_in_memory().unwrap();
        anchored(&store, 100, 110);
        for h in 105..=115 {
            let id = if h <= 112 {
                block(h, 0).id
            } else {
                block(h, 5).id
            };
            store.set_scanned_block(NET, h, &hex::encode(id)).unwrap();
        }
        assert_eq!(
            store.proof_ceiling(NET).unwrap(),
            Some(110),
            "proven up to 110"
        );
        store
            .write_proven(
                NET,
                &block(110, 0),
                &[block(111, 0), block(112, 0), block(113, 0)],
            )
            .unwrap();
        assert_eq!(
            store.proof_ceiling(NET).unwrap(),
            Some(112),
            "113 recorded is another block than 113 proven"
        );
        // The recorded chain is rewound below 112: the ceiling follows.
        store.forget_scanned_blocks_at_or_above(NET, 108).unwrap();
        assert_eq!(store.proof_ceiling(NET).unwrap(), Some(107));
        store.disable_proof(NET).unwrap();
        assert_eq!(store.proof_ceiling(NET).unwrap(), None);
        assert_eq!(store.proven_tip(NET).unwrap(), None, "forgotten");
    }

    #[test]
    fn forgetting_the_anchor_keeps_checking_on() {
        let store = Store::open_in_memory().unwrap();
        anchored(&store, 100, 110);
        store.forget_anchor(NET).unwrap();
        let state = store.proof_network(NET).unwrap().unwrap();
        assert_eq!(state.anchor, None);
        assert_eq!(store.proven_tip(NET).unwrap(), None);
        assert_eq!(store.proof_seed(NET, 2048).unwrap(), None);
        assert_eq!(store.proof_ceiling(NET).unwrap(), Some(0), "still held");
    }
}

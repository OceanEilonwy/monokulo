//! Custom checkout pages' rows: a store's draft, and the stylesheets a page links.

use rusqlite::{params, OptionalExtension};

use super::{ConnectionId, Db, Result};
use crate::views::checkout_style::{stylesheet_hash, CheckoutStyle};

/// How long a draft is kept, unsaved. Long enough to wait on DNS.
pub const DRAFT_SECS: i64 = 7 * 24 * 60 * 60;
/// How long a stylesheet nothing links any more is kept, for pages still open.
const REPLACED_STYLESHEET_SECS: i64 = 7 * 24 * 60 * 60;

/// A store's custom page being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPageDraft {
    pub connection_id: ConnectionId,
    pub template: String,
    pub css: String,
    /// `css` as it's served: scoped and rewritten.
    pub written_css: String,
    pub preview_token: String,
    pub created_at: i64,
}

fn draft_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckoutPageDraft> {
    Ok(CheckoutPageDraft {
        connection_id: row.get(0)?,
        template: row.get(1)?,
        css: row.get(2)?,
        written_css: row.get(3)?,
        preview_token: row.get(4)?,
        created_at: row.get(5)?,
    })
}

const DRAFT_COLUMNS: &str =
    "connection_id, template, css, written_css, preview_token, created_at_utc";

impl Db {
    /// Saves a store's draft, replacing any it had, with the stylesheet it links.
    pub fn save_checkout_page_draft(
        &self,
        id: &ConnectionId,
        template: &str,
        css: &str,
        written_css: &str,
        preview_token: &str,
        now: i64,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // A draft's old stylesheet has no use once it's replaced: its preview link
        // went with it. So a store holds only one, however often it saves.
        let replaced = self.draft_stylesheet_of(id)?;
        tx.execute(
            "INSERT INTO checkout_page_drafts (connection_id, template, css, written_css, preview_token, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (connection_id) DO UPDATE SET template = excluded.template, css = excluded.css,
                written_css = excluded.written_css, preview_token = excluded.preview_token,
                created_at_utc = excluded.created_at_utc",
            params![id, template, css, written_css, preview_token, now],
        )?;
        self.put_stylesheet(id, written_css, now)?;
        if let Some(replaced) = replaced.filter(|replaced| replaced != written_css) {
            self.drop_stylesheet_unless_linked(id, &replaced)?;
        }
        self.prune_stylesheets(id, now)?;
        tx.commit()?;
        Ok(())
    }

    fn draft_stylesheet_of(&self, id: &ConnectionId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT written_css FROM checkout_page_drafts WHERE connection_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The hashes a store's pages link now.
    fn linked_stylesheets(&self, id: &ConnectionId) -> Result<Vec<String>> {
        let style: String = self.conn.query_row(
            "SELECT checkout_style FROM store_connections WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let style = serde_json::from_str::<CheckoutStyle>(&style)
            .ok()
            .and_then(|style| style.css());
        let draft = self.draft_stylesheet_of(id)?;
        Ok(style
            .iter()
            .chain(draft.iter())
            .map(|css| stylesheet_hash(css))
            .collect())
    }

    fn drop_stylesheet_unless_linked(&self, id: &ConnectionId, css: &str) -> Result<()> {
        let hash = stylesheet_hash(css);
        if !self.linked_stylesheets(id)?.contains(&hash) {
            self.conn.execute(
                "DELETE FROM checkout_stylesheets WHERE hash = ?1 AND connection_id = ?2",
                params![hash, id],
            )?;
        }
        Ok(())
    }

    /// A store's draft, unless it has expired.
    pub fn checkout_page_draft(
        &self,
        id: &ConnectionId,
        now: i64,
    ) -> Result<Option<CheckoutPageDraft>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM checkout_page_drafts WHERE connection_id = ?1 AND created_at_utc > ?2"),
                params![id, now - DRAFT_SECS],
                draft_from_row,
            )
            .optional()?)
    }

    /// The draft a preview link names, unless it has expired.
    pub fn checkout_page_draft_by_token(
        &self,
        preview_token: &str,
        now: i64,
    ) -> Result<Option<CheckoutPageDraft>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM checkout_page_drafts WHERE preview_token = ?1 AND created_at_utc > ?2"),
                params![preview_token, now - DRAFT_SECS],
                draft_from_row,
            )
            .optional()?)
    }

    pub fn discard_checkout_page_draft(&self, id: &ConnectionId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let replaced = self.draft_stylesheet_of(id)?;
        tx.execute(
            "DELETE FROM checkout_page_drafts WHERE connection_id = ?1",
            params![id],
        )?;
        if let Some(replaced) = replaced {
            self.drop_stylesheet_unless_linked(id, &replaced)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Keeps `css` for serving by its hash, as of `now`.
    pub(super) fn put_stylesheet(&self, id: &ConnectionId, css: &str, now: i64) -> Result<()> {
        if css.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO checkout_stylesheets (hash, connection_id, css, created_at_utc)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (hash, connection_id) DO UPDATE SET created_at_utc = excluded.created_at_utc",
            params![stylesheet_hash(css), id, css, now],
        )?;
        Ok(())
    }

    /// Drops the store's stylesheets replaced long enough ago, keeping whatever
    /// its pages link now, however old: a page open that long has refreshed.
    pub(super) fn prune_stylesheets(&self, id: &ConnectionId, now: i64) -> Result<()> {
        let keep = self.linked_stylesheets(id)?;
        let old: Vec<String> = self
            .conn
            .prepare(
                "SELECT hash FROM checkout_stylesheets WHERE connection_id = ?1 AND created_at_utc < ?2",
            )?
            .query_map(params![id, now - REPLACED_STYLESHEET_SECS], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for hash in old.iter().filter(|hash| !keep.contains(hash)) {
            self.conn.execute(
                "DELETE FROM checkout_stylesheets WHERE hash = ?1 AND connection_id = ?2",
                params![hash, id],
            )?;
        }
        Ok(())
    }
}

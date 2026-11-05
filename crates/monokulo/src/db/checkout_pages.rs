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

/// A store's custom page in use: the version its domain approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedCheckoutPage {
    pub connection_id: ConnectionId,
    pub template: String,
    pub css: String,
    pub written_css: String,
    /// What its record or file holds.
    pub token: String,
    pub published_at: i64,
    pub last_checked_at: Option<i64>,
    pub failing_since: Option<i64>,
    pub last_error: Option<String>,
    pub turned_off_at: Option<i64>,
    pub turned_off_reason: Option<String>,
}

/// A store's custom page, as the operator's list shows it.
#[derive(Debug, Clone)]
pub struct CustomPageListing {
    pub page: PublishedCheckoutPage,
    pub store_name: String,
    pub domains: Vec<super::StoreDomainRow>,
}

const PAGE_COLUMNS: &str = "connection_id, template, css, written_css, token, published_at_utc, last_checked_at_utc, failing_since_utc, last_error, turned_off_at_utc, turned_off_reason";

fn page_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PublishedCheckoutPage> {
    Ok(PublishedCheckoutPage {
        connection_id: row.get(0)?,
        template: row.get(1)?,
        css: row.get(2)?,
        written_css: row.get(3)?,
        token: row.get(4)?,
        published_at: row.get(5)?,
        last_checked_at: row.get(6)?,
        failing_since: row.get(7)?,
        last_error: row.get(8)?,
        turned_off_at: row.get(9)?,
        turned_off_reason: row.get(10)?,
    })
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
        let published: Option<String> = self
            .conn
            .query_row(
                "SELECT written_css FROM checkout_pages WHERE connection_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(style
            .iter()
            .chain(draft.iter())
            .chain(published.iter())
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

    /// A store's custom page in use, if it has one.
    pub fn published_checkout_page(
        &self,
        id: &ConnectionId,
    ) -> Result<Option<PublishedCheckoutPage>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PAGE_COLUMNS} FROM checkout_pages WHERE connection_id = ?1"),
                params![id],
                page_from_row,
            )
            .optional()?)
    }

    /// Makes `draft` the store's page in use, with its proof just seen. `false`,
    /// changing nothing, if the operator has turned the store's page off.
    pub fn publish_checkout_page(
        &self,
        draft: &CheckoutPageDraft,
        token: &str,
        now: i64,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let id = &draft.connection_id;
        if self
            .published_checkout_page(id)?
            .is_some_and(|page| page.turned_off_at.is_some())
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO checkout_pages (connection_id, template, css, written_css, token, published_at_utc, last_checked_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (connection_id) DO UPDATE SET template = excluded.template, css = excluded.css,
                written_css = excluded.written_css, token = excluded.token,
                published_at_utc = excluded.published_at_utc, last_checked_at_utc = excluded.last_checked_at_utc,
                failing_since_utc = NULL, last_error = NULL",
            params![id, draft.template, draft.css, draft.written_css, token, now],
        )?;
        tx.execute(
            // One saved while the lookup ran is newer work, not this one.
            "DELETE FROM checkout_page_drafts WHERE connection_id = ?1 AND preview_token = ?2",
            params![id, draft.preview_token],
        )?;
        self.put_stylesheet(id, &draft.written_css, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Stops using a store's page, going back to its layout. `false`, changing
    /// nothing, if the operator has turned it off: that stays on the record, so
    /// removing the page can't be how it gets back.
    pub fn stop_using_checkout_page(&self, id: &ConnectionId) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM checkout_pages WHERE connection_id = ?1 AND turned_off_at_utc IS NULL",
            params![id],
        )?;
        Ok(changed == 1)
    }

    /// Records one look at a page's proof, of the version `token`: a slow one mustn't
    /// land on a version published meanwhile. Found clears what was wrong; an answer
    /// without it starts the grace period; no answer changes neither.
    pub fn record_checkout_page_check(
        &self,
        id: &ConnectionId,
        token: &str,
        checked_at: i64,
        answered: bool,
        error: Option<&str>,
    ) -> Result<()> {
        match (answered, error) {
            (true, None) => self.conn.execute(
                "UPDATE checkout_pages SET last_checked_at_utc = ?3, failing_since_utc = NULL, last_error = NULL
                 WHERE connection_id = ?1 AND token = ?2",
                params![id, token, checked_at],
            )?,
            (true, Some(error)) => self.conn.execute(
                "UPDATE checkout_pages SET last_checked_at_utc = ?3, last_error = ?4,
                    failing_since_utc = COALESCE(failing_since_utc, ?3)
                 WHERE connection_id = ?1 AND token = ?2",
                params![id, token, checked_at, error],
            )?,
            (false, error) => self.conn.execute(
                "UPDATE checkout_pages SET last_checked_at_utc = ?3, last_error = ?4
                 WHERE connection_id = ?1 AND token = ?2",
                params![id, token, checked_at, error],
            )?,
        };
        Ok(())
    }

    /// Writes every published page's CSS again from what its domain approved, at each
    /// start: a release that fixes the sanitiser reaches pages already live. CSS it
    /// now refuses stays as it was, and the page doesn't show
    /// (`checkout_pages::parsed`). Returns how many changed.
    pub fn refresh_checkout_pages(&self, now: i64) -> Result<usize> {
        let pages: Vec<PublishedCheckoutPage> = self
            .conn
            .prepare(&format!("SELECT {PAGE_COLUMNS} FROM checkout_pages"))?
            .query_map([], page_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        let mut changed = 0;
        for page in pages {
            let written = checkout_template::Template::load(&page.template)
                .ok()
                .and_then(|template| {
                    checkout_template::css::sanitize(&page.css, template.classes()).ok()
                });
            if let Some(written) = written.filter(|written| *written != page.written_css) {
                self.conn.execute(
                    "UPDATE checkout_pages SET written_css = ?2 WHERE connection_id = ?1",
                    params![page.connection_id, written],
                )?;
                self.put_stylesheet(&page.connection_id, &written, now)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// The operator turns a store's page off, with why, or (`None`) back on.
    /// `false` if it has no page.
    pub fn set_checkout_page_turned_off(
        &self,
        id: &ConnectionId,
        off: Option<(i64, &str)>,
    ) -> Result<bool> {
        let (at, reason) = match off {
            Some((at, reason)) => (Some(at), Some(reason)),
            None => (None, None),
        };
        let changed = self.conn.execute(
            "UPDATE checkout_pages SET turned_off_at_utc = ?2, turned_off_reason = ?3 WHERE connection_id = ?1",
            params![id, at, reason],
        )?;
        Ok(changed == 1)
    }

    /// Every custom page on this instance, for its operator, optionally only the
    /// one store a domain or order points to.
    pub fn list_custom_pages(&self, query: Option<&str>) -> Result<Vec<CustomPageListing>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT p.{}, s.name FROM checkout_pages p JOIN store_connections s ON s.id = p.connection_id ORDER BY s.name",
            PAGE_COLUMNS.replace(", ", ", p.")
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((page_from_row(row)?, row.get::<_, String>(11)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let by_order = match query {
            Some(query) => self
                .get_store_connection_by_order(&super::OrderId::new(query.to_string()))?
                .map(|store| store.id),
            None => None,
        };
        let mut listings = Vec::new();
        for (page, store_name) in rows {
            let domains = self.list_store_domains(&page.connection_id)?;
            let matches = query.is_none_or(|query| {
                by_order.as_ref() == Some(&page.connection_id)
                    || domains
                        .iter()
                        .any(|row| row.domain.contains(&query.to_ascii_lowercase()))
            });
            if matches {
                listings.push(CustomPageListing {
                    page,
                    store_name,
                    domains,
                });
            }
        }
        Ok(listings)
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

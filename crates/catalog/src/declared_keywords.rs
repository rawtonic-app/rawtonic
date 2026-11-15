//! Keywords without photographs (G36/G37, schema 29): the keywords made in
//! the Keyword List (New Keyword) that stay in the library until they are
//! deleted, as Lightroom keeps a keyword whose last photograph lost it. They
//! are listed with a count of 0; a keyword that photographs hold needs no
//! row. Every change returns what it added and removed, so Undo can put it
//! back (`apply_declared_keywords`).
use crate::{CatalogError, Library};
use rawtonic_metadata::keywords;
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// What a change made to the declared keywords: the paths now declared and
/// the paths no longer declared (as spelled in the table).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl DeclaredChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

fn fold(path: &str) -> String {
    path.to_lowercase()
}

/// A change within one transaction: removals first, then additions.
fn apply(
    conn: &rusqlite::Connection,
    remove: &[String],
    add: &[String],
) -> Result<DeclaredChange, CatalogError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut change = DeclaredChange::default();
    for path in remove {
        let path = keywords::normalize(path);
        if path.is_empty() {
            continue;
        }
        let spelled: Option<String> = tx
            .query_row(
                "SELECT path FROM declared_keywords WHERE folded=?",
                [fold(&path)],
                |r| r.get(0),
            )
            .ok();
        if let Some(spelled) = spelled {
            tx.execute(
                "DELETE FROM declared_keywords WHERE folded=?",
                [fold(&path)],
            )?;
            change.removed.push(spelled);
        }
    }
    for path in add {
        let path = keywords::normalize(path);
        if path.is_empty() {
            continue;
        }
        let new = tx.execute(
            "INSERT OR IGNORE INTO declared_keywords(folded, path) VALUES(?,?)",
            params![fold(&path), path],
        )?;
        if new == 1 {
            change.added.push(path);
        }
    }
    tx.commit()?;
    Ok(change)
}

impl Library {
    /// The keywords made without photographs, in name order.
    pub fn declared_keywords(&self) -> Result<Vec<String>, CatalogError> {
        let mut query = self
            .conn
            .prepare("SELECT path FROM declared_keywords ORDER BY folded")?;
        Ok(query
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Makes the keyword `path` (its parents are implied); what it added is
    /// empty when the library has it declared already.
    pub fn declare_keyword(&self, path: &str) -> Result<DeclaredChange, CatalogError> {
        apply(&self.conn, &[], &[path.to_string()])
    }

    /// Takes `remove` off and puts `add` on, ignoring those already so; Undo
    /// and Redo of a change are this with its two lists exchanged.
    pub fn apply_declared_keywords(
        &self,
        remove: &[String],
        add: &[String],
    ) -> Result<DeclaredChange, CatalogError> {
        apply(&self.conn, remove, add)
    }

    /// Renames, moves or merges `from` with the declared keywords under it
    /// to `to` (a name that is declared already merges).
    pub fn rename_declared_keywords(
        &self,
        from: &str,
        to: &str,
    ) -> Result<DeclaredChange, CatalogError> {
        let from = keywords::normalize(from);
        let to = keywords::normalize(to);
        let mut remove = Vec::new();
        let mut add = Vec::new();
        for path in self.declared_keywords()? {
            if let Some(new) = keywords::renamed(&path, &from, &to) {
                remove.push(path);
                add.push(new);
            }
        }
        apply(&self.conn, &remove, &add)
    }

    /// Deletes `path` from the declared keywords; those below it move up one
    /// level (`keywords::without`).
    pub fn delete_declared_keyword(&self, path: &str) -> Result<DeclaredChange, CatalogError> {
        let path = keywords::normalize(path);
        let mut remove = Vec::new();
        let mut add = Vec::new();
        for declared in self.declared_keywords()? {
            if !keywords::is_within(&declared, &path) {
                continue;
            }
            remove.push(declared.clone());
            if let Some(kept) = keywords::without(&declared, &path) {
                add.push(kept);
            }
        }
        apply(&self.conn, &remove, &add)
    }
}

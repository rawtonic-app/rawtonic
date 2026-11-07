// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content search (schema 25, G37): one image embedding per photograph,
//! written by the app's background pass, and the ranking of the library's
//! photographs by how well each matches a text embedding. The catalog
//! knows nothing of the model: it stores what the pass gives (`DIM` values
//! per photograph, made by the model `model`) and scores by cosine. A
//! ranking, never a filter: no score separates "shows it" from "does not"
//! (notes: benchmark-results/srs-search-ceiling-2026-11-03). Nothing here
//! reads or writes a photograph.
use crate::CatalogError;
use crate::Library;
use crate::error::ensure;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::PathBuf;

/// Values per embedding (SigLIP 2 B/16).
pub const DIM: usize = 768;

/// A photograph the pass still has to read.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentWork {
    pub id: String,
    pub path: PathBuf,
    pub fingerprint: String,
}

/// How far the index of `model` is: photographs read (`indexed`), those
/// that could not be read (`failed`) and all present photographs
/// (`total`). A changed file counts as not read until it is read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContentStatus {
    pub indexed: usize,
    pub failed: usize,
    pub total: usize,
}

/// `values` as signed bytes and the scale that gives them back
/// (`value ≈ byte × scale`). All zero (or not finite) has no direction:
/// an error.
pub fn quantize(values: &[f32]) -> Result<(Vec<u8>, f32), CatalogError> {
    ensure!(
        values.len() == DIM,
        "An embedding has {DIM} values, not {}",
        values.len()
    );
    ensure!(
        values.iter().all(|v| v.is_finite()),
        "The embedding has values that are not finite"
    );
    let largest = values.iter().fold(0f32, |m, v| m.max(v.abs()));
    ensure!(largest > 0.0, "The embedding is all zero");
    let scale = largest / 127.0;
    let bytes = values
        .iter()
        .map(|v| (v / scale).round().clamp(-127.0, 127.0) as i8 as u8)
        .collect();
    Ok((bytes, scale))
}

/// The cosine-like score of a stored embedding (`bytes` × `scale`) and
/// `query`: their dot product. The model's embeddings are unit length, so
/// this is their cosine up to the rounding of the bytes.
pub fn score(bytes: &[u8], scale: f32, query: &[f32]) -> f32 {
    let dot: f32 = bytes
        .iter()
        .zip(query)
        .map(|(&b, &q)| (b as i8) as f32 * q)
        .sum();
    dot * scale
}

/// Rows the pass still has to (re)read: none for the photograph, another
/// model's, or one made of the file as it was before.
const STALE: &str =
    "(c.asset_id IS NULL OR c.model!=?1 OR c.fingerprint!=a.fingerprint)";

impl Library {
    /// Present photographs `model` has no current embedding of, newest
    /// registered first, at most `limit`.
    pub fn content_backlog(
        &self,
        model: &str,
        limit: usize,
    ) -> Result<Vec<ContentWork>, CatalogError> {
        let rows: Vec<(String, String, String, bool, String)> = self
            .conn
            .prepare(&format!(
                "SELECT a.id,a.rel_path,r.path,r.external,a.fingerprint FROM assets a
                 JOIN roots r ON r.id=a.root_id
                 LEFT JOIN content_embeddings c ON c.asset_id=a.id
                 WHERE a.missing=0 AND {STALE}
                 ORDER BY a.rowid DESC LIMIT ?2"
            ))?
            .query_map(params![model, i64::try_from(limit)?], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, relative, root, external, fingerprint)| ContentWork {
                id,
                path: self.join_root(root, external, relative),
                fingerprint,
            })
            .collect())
    }

    /// How many photographs `content_backlog` would give without a limit.
    pub fn content_backlog_count(&self, model: &str) -> Result<usize, CatalogError> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM assets a
                 LEFT JOIN content_embeddings c ON c.asset_id=a.id
                 WHERE a.missing=0 AND {STALE}"
            ),
            [model],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n)?)
    }

    /// How far the index of `model` is (`ContentStatus`).
    pub fn content_status(&self, model: &str) -> Result<ContentStatus, CatalogError> {
        let (indexed, failed, total): (i64, i64, i64) = self.conn.query_row(
            "SELECT
               coalesce(sum(c.vector IS NOT NULL AND c.model=?1 AND c.fingerprint=a.fingerprint),0),
               coalesce(sum(c.error IS NOT NULL AND c.model=?1 AND c.fingerprint=a.fingerprint),0),
               count(*)
             FROM assets a LEFT JOIN content_embeddings c ON c.asset_id=a.id
             WHERE a.missing=0",
            [model],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(ContentStatus {
            indexed: usize::try_from(indexed)?,
            failed: usize::try_from(failed)?,
            total: usize::try_from(total)?,
        })
    }

    /// Whether asset `id` has a current embedding of `model` (or a current
    /// failure to read it) for its file as it is now.
    pub fn has_content(&self, id: &str, model: &str) -> Result<bool, CatalogError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM content_embeddings c JOIN assets a ON a.id=c.asset_id
                 WHERE c.asset_id=?1 AND c.model=?2 AND c.fingerprint=a.fingerprint",
                params![id, model],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Records what `model` read from asset `id` when its file had
    /// `fingerprint`: its embedding (`DIM` values), or why it failed.
    /// Replaces an older row. Ignored when the asset is gone.
    pub fn record_content(
        &self,
        id: &str,
        model: &str,
        fingerprint: &str,
        read: Result<&[f32], &str>,
    ) -> Result<(), CatalogError> {
        let (vector, scale, error) = match read {
            Ok(values) => {
                let (bytes, scale) = quantize(values)?;
                (Some(bytes), Some(scale), None)
            }
            Err(error) => (None, None, Some(error)),
        };
        self.conn.execute(
            "INSERT INTO content_embeddings(asset_id,model,fingerprint,scale,vector,error)
             SELECT ?1,?2,?3,?4,?5,?6 WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1)
             ON CONFLICT(asset_id) DO UPDATE SET model=excluded.model,
             fingerprint=excluded.fingerprint,scale=excluded.scale,
             vector=excluded.vector,error=excluded.error",
            params![id, model, fingerprint, scale, vector, error],
        )?;
        Ok(())
    }

    /// Every photograph with a current embedding of `model` (offline ones
    /// too: their embedding stays good), best match for `query` first,
    /// with its score. Ties keep path order.
    pub fn content_ranking(
        &self,
        model: &str,
        query: &[f32],
    ) -> Result<Vec<(PathBuf, f32)>, CatalogError> {
        ensure!(
            query.len() == DIM,
            "A query embedding has {DIM} values, not {}",
            query.len()
        );
        let mut statement = self.conn.prepare(
            "SELECT a.rel_path,r.path,r.external,c.scale,c.vector FROM content_embeddings c
             JOIN assets a ON a.id=c.asset_id JOIN roots r ON r.id=a.root_id
             WHERE c.model=?1 AND c.fingerprint=a.fingerprint AND c.vector IS NOT NULL",
        )?;
        let mut rows = statement.query([model])?;
        let mut ranked = Vec::new();
        while let Some(row) = rows.next()? {
            let bytes = row.get_ref(4)?.as_blob().map_err(rusqlite::Error::from)?;
            if bytes.len() != DIM {
                continue;
            }
            let value = score(bytes, row.get(3)?, query);
            let path = self.join_root(row.get(1)?, row.get(2)?, row.get(0)?);
            ranked.push((path, value));
        }
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(ranked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantized_embeddings_keep_their_direction() {
        let values: Vec<f32> = (0..DIM).map(|i| ((i * 37 % 101) as f32 - 50.0) / 900.0).collect();
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        let unit: Vec<f32> = values.iter().map(|v| v / norm).collect();
        let (bytes, scale) = quantize(&unit).unwrap();
        assert_eq!(bytes.len(), DIM);
        assert!(score(&bytes, scale, &unit) > 0.999);
        assert!(quantize(&vec![0.0; DIM]).is_err());
        assert!(quantize(&[1.0; 3]).is_err());
        assert!(quantize(&[f32::NAN; DIM]).is_err());
    }
}

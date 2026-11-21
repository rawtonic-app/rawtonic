// SPDX-License-Identifier: AGPL-3.0-or-later
//! Assisted culling (schema 30, G35; notes: benchmark-results/
//! cul1-2026-11-21/REPORT.md "CUL2 build plan"): the culling panel's
//! suggestions per photograph — a misfire (lens cap, ground, pocket, motion
//! smear), a document (page, receipt, screenshot), closed eyes — shown as
//! tags with counters and applied only when the photographer accepts them.
//! Nothing here flags, rejects, hides or removes a photograph: accepting is
//! the app's ordinary reject (one undo step), and a dismissed suggestion
//! stays dismissed.
//!
//! Misfire and document scores come from the photograph's content embedding
//! (`content`, SigLIP 2): the best cosine to the frozen prompts below, as
//! SigLIP's calibrated tag score `sigmoid(scale · cos + bias)`. The app
//! gives the prompt embeddings (its text encoder) and the image embedding as
//! the model gave it when the content pass reads the photograph; the catalog
//! stores the two scores (`record_cull`). Eyes come from
//! `face_eyes` (closed or partly, the photographer's call winning).
use crate::CatalogError;
use crate::Library;
use crate::error::{bail, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::Serialize;
use std::collections::HashMap;

/// Prompts of a misfire, frozen before any score was computed (CUL1
/// `data/prompts.json`, 2026-11-21). Changing one changes the prompt set's
/// id, so every photograph is scored again — and the thresholds below were
/// tuned for exactly these.
pub const MISFIRE_PROMPTS: [&str; 7] = [
    "a photo taken by accident",
    "a completely black photo",
    "a blurry photo of the ground",
    "a photo of the floor and feet taken by mistake",
    "a dark blurry photo taken inside a pocket",
    "a heavily motion-blurred photo",
    "a photo taken with the lens cap on",
];

/// Prompts of a document (frozen with `MISFIRE_PROMPTS`).
pub const DOCUMENT_PROMPTS: [&str; 6] = [
    "a photo of a document",
    "a scanned page of text",
    "a screenshot",
    "a photo of a receipt",
    "a page of printed text",
    "a computer screen showing text",
];

/// Least misfire score that suggests a misfire: chosen on CUL1's tuning
/// half (max F1), held-out P 0.77 R 0.71 (REPORT.md "Bottom line").
pub const MISFIRE_THRESHOLD: f32 = 0.01153;
/// Least document score that suggests a document (CUL1 tuning half;
/// held-out P 1.00 R 0.75).
pub const DOCUMENT_THRESHOLD: f32 = 0.02835;
/// Auto-Stack's default least cosine of two consecutive photographs'
/// embeddings to share a stack (CUL1 tuning half with a 2 s gap; held-out
/// pairs P 0.88 R 0.90).
pub const STACK_SIMILARITY: f32 = 0.78;

/// The suggestions, by the names the app and the search rules use.
pub const CRITERIA: [&str; 3] = ["misfire", "document", "eyesClosed"];

/// The prompt embeddings the scores are made with.
#[derive(Debug, Clone)]
pub struct PromptSet {
    /// Names the text model and the prompts (a new one scores everything
    /// again).
    pub id: String,
    /// Unit-length text embeddings, one per prompt.
    pub misfire: Vec<Vec<f32>>,
    pub document: Vec<Vec<f32>>,
    /// The model's calibration: `sigmoid(scale · cos + bias)`.
    pub logit_scale: f32,
    pub logit_bias: f32,
}

impl PromptSet {
    fn tag_score(&self, best_cosine: f32) -> f32 {
        1.0 / (1.0 + (-(self.logit_scale * best_cosine + self.logit_bias)).exp())
    }

    /// The (misfire, document) scores of an image embedding as the model
    /// gave it (unit length, f32). Never from the catalog's stored bytes:
    /// the calibration's slope (×112) turns their rounding into score
    /// changes of up to ~46 % (CUL2 measured, p95 24 %).
    pub fn scores(&self, embedding: &[f32]) -> (f32, f32) {
        let best = |prompts: &[Vec<f32>]| {
            prompts
                .iter()
                .map(|p| p.iter().zip(embedding).map(|(a, b)| a * b).sum::<f32>())
                .fold(f32::NEG_INFINITY, f32::max)
        };
        (
            self.tag_score(best(&self.misfire)),
            self.tag_score(best(&self.document)),
        )
    }

    fn check(&self) -> Result<(), CatalogError> {
        ensure!(
            !self.misfire.is_empty() && !self.document.is_empty(),
            "A prompt set needs misfire and document prompts"
        );
        ensure!(
            self.misfire
                .iter()
                .chain(&self.document)
                .all(|p| p.len() == crate::content::DIM && p.iter().all(|v| v.is_finite())),
            "A prompt embedding has {} finite values",
            crate::content::DIM
        );
        Ok(())
    }
}

/// SQL test for an asset `a` without current scores of model `?1` and the
/// score id `?2`, whose reading by `?1` has not failed for its file now.
const CULL_STALE: &str = "(NOT EXISTS(SELECT 1 FROM cull_scores s WHERE s.asset_id=a.id \
     AND s.model=?2 AND s.fingerprint=a.fingerprint) \
     AND NOT EXISTS(SELECT 1 FROM content_embeddings c WHERE c.asset_id=a.id \
     AND c.model=?1 AND c.fingerprint=a.fingerprint AND c.error IS NOT NULL))";

/// The id scores of `model`'s embeddings made with prompt set `set` carry.
fn score_model(model: &str, set: &str) -> String {
    format!("{model}/{set}")
}

/// SQL test for an asset `a` that `criterion` suggests (score at or above
/// its threshold for the file as it is, or closed eyes) and the
/// photographer has not dismissed.
pub(crate) fn suggested_sql(criterion: &str) -> Result<String, CatalogError> {
    let test = match criterion {
        "misfire" => format!(
            "EXISTS(SELECT 1 FROM cull_scores s WHERE s.asset_id=a.id \
             AND s.fingerprint=a.fingerprint AND s.misfire>={MISFIRE_THRESHOLD})"
        ),
        "document" => format!(
            "EXISTS(SELECT 1 FROM cull_scores s WHERE s.asset_id=a.id \
             AND s.fingerprint=a.fingerprint AND s.document>={DOCUMENT_THRESHOLD})"
        ),
        "eyesClosed" => crate::face_eyes::has_face_sql("closed"),
        other => bail!("Unknown culling suggestion {other}"),
    };
    Ok(format!(
        "({test} AND NOT EXISTS(SELECT 1 FROM cull_dismissed d \
         WHERE d.asset_id=a.id AND d.criterion='{criterion}'))"
    ))
}

/// What the panel shows of one photograph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CullPhoto {
    /// Criteria suggesting it (`CRITERIA` order), dismissed ones left out.
    pub suggested: Vec<&'static str>,
    /// Criteria the photographer dismissed.
    pub dismissed: Vec<String>,
    /// Its misfire and document scores are current (else not read yet).
    pub scored: bool,
    /// It could not be read again for its scores (`record_cull_failure`).
    pub failed: bool,
}

impl Library {
    /// Present photographs without current scores of `model` and prompt
    /// set `set` whose reading has not failed for their file as it is now:
    /// the content pass reads them (again) and scores them, newest
    /// registered first, at most `limit`. A superset of `content_backlog`.
    pub fn cull_backlog(
        &self,
        model: &str,
        set: &str,
        limit: usize,
    ) -> Result<Vec<crate::content::ContentWork>, CatalogError> {
        let rows: Vec<(String, String, String, bool, String)> = self
            .conn
            .prepare(&format!(
                "SELECT a.id,a.rel_path,r.path,r.external,a.fingerprint FROM assets a
                 JOIN roots r ON r.id=a.root_id
                 WHERE a.missing=0 AND {CULL_STALE}
                 ORDER BY a.rowid DESC LIMIT ?3"
            ))?
            .query_map(
                params![model, score_model(model, set), i64::try_from(limit)?],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(
                |(id, relative, root, external, fingerprint)| crate::content::ContentWork {
                    id,
                    path: self.join_root(root, external, relative),
                    fingerprint,
                },
            )
            .collect())
    }

    /// How many photographs `cull_backlog` would give without a limit.
    pub fn cull_backlog_count(&self, model: &str, set: &str) -> Result<usize, CatalogError> {
        let n: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM assets a WHERE a.missing=0 AND {CULL_STALE}"),
            params![model, score_model(model, set)],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n)?)
    }

    /// Whether asset `id` has current scores of `model` and `set` (or a
    /// current failure to read it for them or for its embedding).
    pub fn has_cull(&self, id: &str, model: &str, set: &str) -> Result<bool, CatalogError> {
        let stale: bool = self.conn.query_row(
            &format!("SELECT {CULL_STALE} FROM assets a WHERE a.id=?3"),
            params![model, score_model(model, set), id],
            |r| r.get(0),
        )?;
        Ok(!stale)
    }

    /// Records the scores of asset `id` from `embedding`, the image
    /// embedding `model` read from its file when it had `fingerprint`, made
    /// with prompt set `set`. Replaces older scores; ignored when the asset
    /// is gone.
    pub fn record_cull(
        &self,
        id: &str,
        model: &str,
        set: &PromptSet,
        fingerprint: &str,
        embedding: &[f32],
    ) -> Result<(), CatalogError> {
        set.check()?;
        ensure!(
            embedding.len() == crate::content::DIM && embedding.iter().all(|v| v.is_finite()),
            "An embedding has {} finite values",
            crate::content::DIM
        );
        let (misfire, document) = set.scores(embedding);
        self.conn.execute(
            "INSERT INTO cull_scores(asset_id,model,fingerprint,misfire,document,error)
             SELECT ?1,?2,?3,?4,?5,NULL WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1)
             ON CONFLICT(asset_id) DO UPDATE SET model=excluded.model,
             fingerprint=excluded.fingerprint,misfire=excluded.misfire,
             document=excluded.document,error=NULL",
            params![
                id,
                score_model(model, &set.id),
                fingerprint,
                misfire,
                document
            ],
        )?;
        Ok(())
    }

    /// Records that asset `id`, whose search embedding of `model` is kept,
    /// could not be read again for its scores of `set` when its file had
    /// `fingerprint` (`error`): its embedding stays, and it is not asked for
    /// again until the file, model or prompt set changes.
    pub fn record_cull_failure(
        &self,
        id: &str,
        model: &str,
        set: &str,
        fingerprint: &str,
        error: &str,
    ) -> Result<(), CatalogError> {
        self.conn.execute(
            "INSERT INTO cull_scores(asset_id,model,fingerprint,misfire,document,error)
             SELECT ?1,?2,?3,NULL,NULL,?4 WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1)
             ON CONFLICT(asset_id) DO UPDATE SET model=excluded.model,
             fingerprint=excluded.fingerprint,misfire=NULL,document=NULL,
             error=excluded.error",
            params![id, score_model(model, set), fingerprint, error],
        )?;
        Ok(())
    }

    /// Forgets every failure to read a photograph again for its scores, so
    /// the content pass tries them once more (the library was opened). The
    /// search embeddings stay; returns how many.
    pub fn clear_cull_failures(&self) -> Result<usize, CatalogError> {
        Ok(self
            .conn
            .execute("DELETE FROM cull_scores WHERE error IS NOT NULL", [])?)
    }

    /// The stored (misfire, document) scores of asset `id` when current.
    pub fn cull_scores(&self, id: &str) -> Result<Option<(f32, f32)>, CatalogError> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT s.misfire,s.document FROM cull_scores s JOIN assets a ON a.id=s.asset_id
                 WHERE s.asset_id=? AND s.fingerprint=a.fingerprint AND s.error IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// What the panel shows of each of `ids` (unknown ids left out).
    pub fn cull_photos(&self, ids: &[String]) -> Result<HashMap<String, CullPhoto>, CatalogError> {
        let tests = CRITERIA
            .iter()
            .map(|c| suggested_sql(c))
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        let sql = format!(
            "SELECT a.id,{tests},
               EXISTS(SELECT 1 FROM cull_scores s WHERE s.asset_id=a.id
                      AND s.fingerprint=a.fingerprint AND s.error IS NULL),
               (SELECT group_concat(d.criterion) FROM cull_dismissed d WHERE d.asset_id=a.id),
               EXISTS(SELECT 1 FROM cull_scores s WHERE s.asset_id=a.id
                      AND s.fingerprint=a.fingerprint AND s.error IS NOT NULL)
             FROM assets a JOIN (SELECT DISTINCT value AS id FROM json_each(?)) j ON j.id=a.id"
        );
        let mut out = HashMap::new();
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query([serde_json::to_string(ids)?])?;
        while let Some(row) = rows.next()? {
            let mut photo = CullPhoto::default();
            for (k, criterion) in CRITERIA.iter().enumerate() {
                if row.get::<_, bool>(k + 1)? {
                    photo.suggested.push(criterion);
                }
            }
            photo.scored = row.get(CRITERIA.len() + 1)?;
            let dismissed: Option<String> = row.get(CRITERIA.len() + 2)?;
            photo.dismissed = dismissed
                .map(|d| d.split(',').map(str::to_string).collect())
                .unwrap_or_default();
            photo.dismissed.sort();
            photo.failed = row.get(CRITERIA.len() + 3)?;
            out.insert(row.get(0)?, photo);
        }
        Ok(out)
    }

    /// Dismisses (`dismissed`) or brings back criterion `criterion`'s
    /// suggestion for `ids`, in one transaction; returns the ids whose state
    /// changed.
    pub fn set_cull_dismissed(
        &self,
        ids: &[String],
        criterion: &str,
        dismissed: bool,
    ) -> Result<Vec<String>, CatalogError> {
        ensure!(
            CRITERIA.contains(&criterion),
            "Unknown culling suggestion {criterion}"
        );
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut changed = Vec::new();
        for id in ids {
            let n = if dismissed {
                tx.execute(
                    "INSERT OR IGNORE INTO cull_dismissed(asset_id,criterion)
                     SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM assets WHERE id=?1)",
                    params![id, criterion],
                )?
            } else {
                tx.execute(
                    "DELETE FROM cull_dismissed WHERE asset_id=?1 AND criterion=?2",
                    params![id, criterion],
                )?
            };
            if n > 0 && !changed.contains(id) {
                changed.push(id.clone());
            }
        }
        tx.commit()?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_criterion_has_a_test_and_others_are_refused() {
        for criterion in CRITERIA {
            assert!(suggested_sql(criterion).unwrap().contains(criterion));
        }
        assert!(suggested_sql("exposure").is_err());
    }

    #[test]
    fn the_tag_score_is_the_calibrated_sigmoid() {
        let set = PromptSet {
            id: "t".into(),
            misfire: vec![],
            document: vec![],
            logit_scale: 112.6689,
            logit_bias: -16.771725,
        };
        // sigmoid(0) where the cosine cancels the bias.
        assert!((set.tag_score(16.771725 / 112.6689) - 0.5).abs() < 1e-5);
        assert!(set.tag_score(0.0) < MISFIRE_THRESHOLD);
    }
}

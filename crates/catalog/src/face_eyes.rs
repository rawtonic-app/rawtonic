//! Eyes per face for culling (schema 21; eye focus since schema 25): what
//! the app's background pass read from each photograph
//! (`rawtonic_faces::eyes` and `::focus`, kept here as text so the catalog
//! does not depend on the face models) and the photographer's own calls
//! per face, which win over the reading everywhere. Nothing here reads,
//! rejects or removes a photograph.
use crate::Library;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A reader's calls (`rawtonic_faces::eyes::EyeState` by name).
pub const STATES: [&str; 4] = ["open", "closed", "notVisible", "uncertain"];
/// The calls a photographer can make (never "uncertain").
pub const OVERRIDES: [&str; 3] = ["open", "closed", "notVisible"];
/// A reader's eye-focus calls (`rawtonic_faces::focus::FocusState`).
pub const FOCUS_STATES: [&str; 3] = ["sharp", "soft", "uncertain"];
/// The eye-focus calls a photographer can make.
pub const FOCUS_OVERRIDES: [&str; 2] = ["sharp", "soft"];

/// One face's eyes as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EyeFace {
    /// x, y, width, height as fractions of the upright, uncropped photo.
    pub bbox: [f32; 4],
    pub state: String,
    pub ear: Option<f32>,
    pub accessories: Option<f32>,
    /// Eye focus as read; `None` from a reader before eye focus.
    #[serde(default)]
    pub focus: Option<String>,
    /// Its measurement (edge sigma at a fixed eye distance, px).
    #[serde(default)]
    pub focus_sigma: Option<f32>,
}

/// One face's eyes in the catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEyeFace {
    /// Left to right from 0.
    pub face: u32,
    #[serde(flatten)]
    pub read: EyeFace,
    /// The photographer's call, if any.
    pub override_state: Option<String>,
    /// The photographer's eye-focus call, if any.
    pub focus_override: Option<String>,
}

impl StoredEyeFace {
    /// The call that counts: the photographer's, else the reader's.
    pub fn effective(&self) -> &str {
        self.override_state.as_deref().unwrap_or(&self.read.state)
    }

    /// The eye-focus call that counts: the photographer's, else the
    /// reader's (`None`: not read).
    pub fn effective_focus(&self) -> Option<&str> {
        self.focus_override
            .as_deref()
            .or(self.read.focus.as_deref())
    }
}

/// A photograph's eyes: `None` while not read (or read by an older
/// reader, or the file changed since), else its faces (perhaps none).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoEyes {
    pub faces: Option<Vec<StoredEyeFace>>,
    /// Why the photograph could not be read.
    pub error: Option<String>,
}

/// A photograph the pass still has to read.
#[derive(Debug, Clone, PartialEq)]
pub struct EyeWork {
    pub id: String,
    pub path: PathBuf,
    pub fingerprint: String,
}

/// Overlap of two boxes (intersection over union).
fn overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

/// Least overlap for a new reading's face to be the one a call was made on.
const SAME_FACE: f32 = 0.5;

/// SQL test for an asset `a` with a face whose call that counts is `state`.
pub(crate) fn has_face_sql(state: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM face_eyes e WHERE e.asset_id=a.id \
         AND coalesce(e.override_state, e.state)='{state}')"
    )
}

/// SQL test for an asset `a` with a face whose eye-focus call that counts
/// is `state`.
pub(crate) fn has_focus_sql(state: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM face_eyes e WHERE e.asset_id=a.id \
         AND coalesce(e.focus_override, e.focus)='{state}')"
    )
}

impl Library {
    /// Present photographs whose eyes are not read by reader `version`
    /// with their current fingerprint, newest registered first, at most
    /// `limit`.
    pub fn eye_backlog(&self, version: u32, limit: usize) -> Result<Vec<EyeWork>> {
        let rows: Vec<(String, String, String, bool, String)> = self
            .conn
            .prepare(
                "SELECT a.id,a.rel_path,r.path,r.external,a.fingerprint FROM assets a
                 JOIN roots r ON r.id=a.root_id
                 LEFT JOIN eye_reads e ON e.asset_id=a.id
                 WHERE a.missing=0 AND (e.asset_id IS NULL OR e.version<? OR e.fingerprint!=a.fingerprint)
                 ORDER BY a.rowid DESC LIMIT ?",
            )?
            .query_map(params![version, i64::try_from(limit)?], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, relative, root, external, fingerprint)| EyeWork {
                id,
                path: self.join_root(root, external, relative),
                fingerprint,
            })
            .collect())
    }

    /// How many photographs `eye_backlog` would give without a limit.
    pub fn eye_backlog_count(&self, version: u32) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM assets a LEFT JOIN eye_reads e ON e.asset_id=a.id
             WHERE a.missing=0 AND (e.asset_id IS NULL OR e.version<? OR e.fingerprint!=a.fingerprint)",
            [version],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n)?)
    }

    /// Records what reader `version` read from asset `id` when its file had
    /// `fingerprint`: its faces left to right, or why it failed. The
    /// photographer's calls (eyes and eye focus) go to the same faces of the
    /// new reading (boxes overlapping by half or more); calls whose face is
    /// not found again are dropped with it. Ignored when the asset is gone.
    pub fn record_eyes(
        &self,
        id: &str,
        version: u32,
        fingerprint: &str,
        read: Result<&[EyeFace], &str>,
    ) -> Result<()> {
        if let Ok(faces) = read {
            for face in faces {
                ensure!(
                    STATES.contains(&face.state.as_str()),
                    "Unknown eye state {}",
                    face.state
                );
                if let Some(focus) = &face.focus {
                    ensure!(
                        FOCUS_STATES.contains(&focus.as_str()),
                        "Unknown eye focus {focus}"
                    );
                }
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        let present: bool = tx
            .query_row("SELECT 1 FROM assets WHERE id=?", [id], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !present {
            return Ok(());
        }
        type Calls = (Option<String>, Option<String>);
        let calls: Vec<([f32; 4], Calls)> = tx
            .prepare(
                "SELECT x,y,w,h,override_state,focus_override FROM face_eyes
                 WHERE asset_id=? AND (override_state IS NOT NULL OR focus_override IS NOT NULL)",
            )?
            .query_map([id], |r| {
                Ok((
                    [r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?],
                    (r.get(4)?, r.get(5)?),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute("DELETE FROM face_eyes WHERE asset_id=?", [id])?;
        let error = read.err();
        if let Ok(faces) = read {
            let mut insert = tx.prepare(
                "INSERT INTO face_eyes(asset_id,face,x,y,w,h,state,ear,accessories,override_state,
                 focus,focus_sigma,focus_override)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )?;
            let mut taken = vec![false; faces.len()];
            let mut carried: Vec<Option<&Calls>> = vec![None; faces.len()];
            for (bbox, call) in &calls {
                let best = faces
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !taken[*i])
                    .map(|(i, f)| (i, overlap(*bbox, f.bbox)))
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, o)) = best
                    && o >= SAME_FACE
                {
                    taken[i] = true;
                    carried[i] = Some(call);
                }
            }
            for (i, face) in faces.iter().enumerate() {
                let [x, y, w, h] = face.bbox;
                let (eyes_call, focus_call) = match carried[i] {
                    Some((eyes, focus)) => (eyes.as_deref(), focus.as_deref()),
                    None => (None, None),
                };
                insert.execute(params![
                    id,
                    i as i64,
                    x,
                    y,
                    w,
                    h,
                    face.state,
                    face.ear,
                    face.accessories,
                    eyes_call,
                    face.focus,
                    face.focus_sigma,
                    focus_call
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO eye_reads(asset_id,version,fingerprint,error) VALUES(?,?,?,?)
             ON CONFLICT(asset_id) DO UPDATE SET version=excluded.version,
             fingerprint=excluded.fingerprint,error=excluded.error",
            params![id, version, fingerprint, error],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The eyes of asset `id` as read by reader `version` (or a newer one).
    pub fn photo_eyes(&self, id: &str, version: u32) -> Result<PhotoEyes> {
        Ok(self
            .photos_eyes(&[id.to_string()], version)?
            .remove(id)
            .unwrap_or(PhotoEyes {
                faces: None,
                error: None,
            }))
    }

    /// `photo_eyes` of several assets at once (unknown ids left out).
    pub fn photos_eyes(&self, ids: &[String], version: u32) -> Result<HashMap<String, PhotoEyes>> {
        let mut out = HashMap::new();
        let mut read = self.conn.prepare(
            "SELECT e.error FROM eye_reads e JOIN assets a ON a.id=e.asset_id
             WHERE e.asset_id=? AND e.version>=? AND e.fingerprint=a.fingerprint",
        )?;
        let mut faces = self.conn.prepare(
            "SELECT face,x,y,w,h,state,ear,accessories,override_state,focus,focus_sigma,
             focus_override FROM face_eyes
             WHERE asset_id=? ORDER BY face",
        )?;
        let mut known = self.conn.prepare("SELECT 1 FROM assets WHERE id=?")?;
        for id in ids {
            if !known.exists([id])? {
                continue;
            }
            let error: Option<Option<String>> = read
                .query_row(params![id, version], |r| r.get(0))
                .optional()?;
            let eyes = match error {
                None => PhotoEyes {
                    faces: None,
                    error: None,
                },
                Some(Some(error)) => PhotoEyes {
                    faces: None,
                    error: Some(error),
                },
                Some(None) => PhotoEyes {
                    faces: Some(
                        faces
                            .query_map([id], |r| {
                                Ok(StoredEyeFace {
                                    face: r.get(0)?,
                                    read: EyeFace {
                                        bbox: [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                                        state: r.get(5)?,
                                        ear: r.get(6)?,
                                        accessories: r.get(7)?,
                                        focus: r.get(9)?,
                                        focus_sigma: r.get(10)?,
                                    },
                                    override_state: r.get(8)?,
                                    focus_override: r.get(11)?,
                                })
                            })?
                            .collect::<rusqlite::Result<_>>()?,
                    ),
                    error: None,
                },
            };
            out.insert(id.clone(), eyes);
        }
        Ok(out)
    }

    /// Sets (or with `None` clears) the photographer's call on face `face`
    /// of asset `id`. Fails for a face the photograph does not have.
    pub fn set_eye_override(&self, id: &str, face: u32, call: Option<&str>) -> Result<()> {
        if let Some(call) = call {
            ensure!(OVERRIDES.contains(&call), "Unknown eye call {call}");
        }
        let changed = self.conn.execute(
            "UPDATE face_eyes SET override_state=? WHERE asset_id=? AND face=?",
            params![call, id, face],
        )?;
        ensure!(changed == 1, "This photograph has no face {face}");
        Ok(())
    }

    /// Sets (or with `None` clears) the photographer's eye-focus call on
    /// face `face` of asset `id`. Fails for a face the photograph does not
    /// have.
    pub fn set_focus_override(&self, id: &str, face: u32, call: Option<&str>) -> Result<()> {
        if let Some(call) = call {
            ensure!(
                FOCUS_OVERRIDES.contains(&call),
                "Unknown eye focus call {call}"
            );
        }
        let changed = self.conn.execute(
            "UPDATE face_eyes SET focus_override=? WHERE asset_id=? AND face=?",
            params![call, id, face],
        )?;
        ensure!(changed == 1, "This photograph has no face {face}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::overlap;

    #[test]
    fn overlap_is_intersection_over_union() {
        assert_eq!(overlap([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]), 1.0);
        assert_eq!(overlap([0.0, 0.0, 1.0, 1.0], [2.0, 2.0, 1.0, 1.0]), 0.0);
        assert!((overlap([0.0, 0.0, 2.0, 1.0], [1.0, 0.0, 2.0, 1.0]) - 1.0 / 3.0).abs() < 1e-6);
    }
}

//! Capture facts of each photograph (camera, lens, exposure, capture time)
//! as the library's search finds them. The catalog reads no files' EXIF
//! itself: whoever adds a photograph passes what it read
//! (`PlannedAsset::with_facts`; verified ingest does), and a background
//! pass (`fill_facts`) reads the photographs added without them, each file
//! once, with a reader the caller brings (`rawtonic_ingest::capture_facts`
//! in the app and the CLI).
use crate::Library;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// What a photograph's EXIF says about its capture. Empty fields: the file
/// does not say.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureFacts {
    /// Local time as the camera recorded it, `YYYY-MM-DD HH:MM:SS`.
    pub captured: Option<String>,
    pub make: Option<String>,
    /// Camera model.
    pub model: Option<String>,
    pub lens: Option<String>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    /// F-number.
    pub aperture: Option<f64>,
    pub iso: Option<u32>,
    /// Seconds.
    pub exposure: Option<f64>,
    /// The IPTC Digital Source Type the file's own XMP states (a generator
    /// or another editor marked it), as its IPTC code
    /// (`rawtonic_metadata::provenance::source_type_code`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    /// The rating the camera wrote into the file (1–5): the photograph's
    /// rating while nobody has set its metadata (`write_facts`). Not kept
    /// with the facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_rating: Option<u8>,
    /// EXIF ExposureMode: 0 auto, 1 manual, 2 auto bracket (Auto-Stack's
    /// bracket rule, `stacks::auto_groups`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_mode: Option<u8>,
    /// EXIF ExposureBiasValue (EV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_bias: Option<f64>,
    /// The maker's frame number within a continuous-drive burst (1 for its
    /// first frame), where the reader knows the maker's field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u32>,
}

impl CaptureFacts {
    /// Values a damaged or odd file gives that no search could mean
    /// (negative or non-finite numbers, overlong text) are dropped.
    fn sanitized(&self) -> Self {
        let text = |v: &Option<String>| {
            v.as_ref()
                .map(|t| {
                    t.trim()
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(255)
                        .collect::<String>()
                })
                .filter(|t| !t.is_empty())
        };
        let positive = |v: Option<f64>| v.filter(|n| n.is_finite() && *n > 0.0);
        Self {
            captured: self
                .captured
                .as_ref()
                .filter(|t| chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S").is_ok())
                .cloned(),
            make: text(&self.make),
            model: text(&self.model),
            lens: text(&self.lens),
            focal_length: positive(self.focal_length),
            aperture: positive(self.aperture),
            iso: self.iso.filter(|n| *n > 0),
            exposure: positive(self.exposure),
            source_type: text(
                &self
                    .source_type
                    .as_deref()
                    .map(rawtonic_metadata::provenance::source_type_code),
            ),
            camera_rating: self.camera_rating.filter(|r| (1..=5).contains(r)),
            exposure_mode: self.exposure_mode.filter(|m| *m <= 2),
            exposure_bias: self
                .exposure_bias
                .filter(|v| v.is_finite() && v.abs() <= 64.0),
            sequence: self.sequence.filter(|n| (1..=1_000_000).contains(n)),
        }
    }
}

/// Stores `facts` as asset `id`'s and marks its file read. The camera's
/// rating becomes the photograph's only while its metadata was never set
/// (no sidecar adopted, nobody rated, tagged or edited it): a rating anyone
/// gave is never replaced. Marked as the camera's (`from_camera`), the
/// photograph still counts as untouched, so a sidecar found later is
/// adopted over it. Returns whether the asset exists and the camera's
/// rating if it was taken.
pub(crate) fn write_facts(
    conn: &Connection,
    id: &str,
    facts: &CaptureFacts,
) -> Result<(bool, Option<u8>)> {
    let facts = facts.sanitized();
    let written = conn.execute(
        "UPDATE assets SET capture_time=?,make=?,camera=?,lens=?,focal_length=?,aperture=?,iso=?,
                exposure=?,source_type=?,exposure_mode=?,exposure_bias=?,sequence=?,facts_read=1
         WHERE id=?",
        params![
            facts.captured,
            facts.make.unwrap_or_default(),
            facts.model.unwrap_or_default(),
            facts.lens.unwrap_or_default(),
            facts.focal_length,
            facts.aperture,
            facts.iso,
            facts.exposure,
            facts.source_type.unwrap_or_default(),
            facts.exposure_mode,
            facts.exposure_bias,
            facts.sequence,
            id
        ],
    )? == 1;
    if written
        && let Some(rating) = facts.camera_rating
        && crate::add::untouched(conn, id)?
    {
        let metadata = rawtonic_metadata::Metadata {
            rating,
            ..Default::default()
        };
        // The rating is the file's own: the XMP has nothing to catch up
        // on, so its state stays as it was.
        let status: String = conn.query_row(
            "SELECT xmp_status FROM metadata WHERE asset_id=?",
            [id],
            |r| r.get(0),
        )?;
        crate::write_metadata(conn, id, &metadata)?;
        conn.execute(
            "UPDATE metadata SET from_camera=1,xmp_status=? WHERE asset_id=?",
            params![status, id],
        )?;
        return Ok((true, Some(rating)));
    }
    Ok((written, None))
}

/// Photographs that took their camera's rating, with it (`fill_facts`).
pub type Rated = Vec<(PathBuf, u8)>;

impl Library {
    /// Records what asset `id`'s file says about its capture. Returns
    /// whether the asset exists.
    pub fn record_facts(&self, id: &str, facts: &CaptureFacts) -> Result<bool> {
        Ok(write_facts(&self.conn, id, facts)?.0)
    }

    /// Asset `id`'s capture facts, and whether its file was read for them.
    pub fn facts(&self, id: &str) -> Result<Option<(CaptureFacts, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT capture_time,make,camera,lens,focal_length,aperture,iso,exposure,facts_read,
                        source_type,exposure_mode,exposure_bias,sequence
                 FROM assets WHERE id=?",
                [id],
                |r| {
                    let text = |i: usize| -> rusqlite::Result<Option<String>> {
                        Ok(r.get::<_, Option<String>>(i)?.filter(|t| !t.is_empty()))
                    };
                    Ok((
                        CaptureFacts {
                            captured: text(0)?,
                            make: text(1)?,
                            model: text(2)?,
                            lens: text(3)?,
                            focal_length: r.get(4)?,
                            aperture: r.get(5)?,
                            iso: r.get(6)?,
                            exposure: r.get(7)?,
                            source_type: text(9)?,
                            camera_rating: None,
                            exposure_mode: r.get(10)?,
                            exposure_bias: r.get(11)?,
                            sequence: r.get(12)?,
                        },
                        r.get(8)?,
                    ))
                },
            )
            .optional()?)
    }

    /// How many present photographs were not read for their facts yet.
    pub fn facts_backlog(&self) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM assets WHERE facts_read=0 AND missing=0",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// Reads up to `limit` present photographs not read yet that come after
    /// `cursor` (registration order; the cursor moves past every one looked
    /// at, so a file `read` cannot read is not tried again in this pass),
    /// with `pause` between files, until `cancel` is set. `read` returns
    /// `None` when the file cannot be read (it is tried again by a later
    /// pass) and the facts otherwise (empty for a file without EXIF).
    /// Facts with a camera rating are recorded while holding what `turn`
    /// returns: the caller's turn among metadata writers that read a
    /// photograph's metadata, change it and write it back, so none of them
    /// writes back what it read before the camera's rating was taken.
    /// Returns how many were looked at and how many recorded, and the
    /// photographs that took their camera's rating (`write_facts`) with it.
    pub fn fill_facts<T>(
        &self,
        cursor: &mut i64,
        limit: usize,
        pause: Duration,
        cancel: &AtomicBool,
        read: &dyn Fn(&Path) -> Option<CaptureFacts>,
        turn: &dyn Fn() -> T,
    ) -> Result<(usize, usize, Rated)> {
        let rows: Vec<(i64, String, String, String, bool)> = self
            .conn
            .prepare(
                "SELECT a.rowid,a.id,a.rel_path,r.path,r.external FROM assets a JOIN roots r ON r.id=a.root_id
                 WHERE a.facts_read=0 AND a.missing=0 AND a.rowid>? ORDER BY a.rowid LIMIT ?",
            )?
            .query_map(params![*cursor, i64::try_from(limit)?], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let (mut looked, mut recorded, mut rated) = (0, 0, Vec::new());
        for (rowid, id, relative, root, external) in rows {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            *cursor = rowid;
            looked += 1;
            let path: PathBuf = self.join_root(root, external, relative);
            let Some(facts) = read(&path) else {
                continue;
            };
            let _turn = facts.camera_rating.is_some().then(turn);
            // Only while it is still unread: a newer record (an ingest's)
            // is never replaced by the pass.
            let tx = self.conn.unchecked_transaction()?;
            let unread: bool = tx
                .query_row("SELECT facts_read=0 FROM assets WHERE id=?", [&id], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(false);
            if unread {
                let (written, rating) = write_facts(&tx, &id, &facts)?;
                recorded += usize::from(written);
                if let Some(rating) = rating {
                    rated.push((path, rating));
                }
            }
            tx.commit()?;
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
        Ok((looked, recorded, rated))
    }

    /// Reads every present photograph not read yet, at once (tests, the
    /// CLI: no other metadata writer runs). Returns how many were recorded.
    pub fn fill_all_facts(
        &self,
        cancel: &AtomicBool,
        read: &dyn Fn(&Path) -> Option<CaptureFacts>,
    ) -> Result<usize> {
        let mut cursor = 0;
        let mut recorded = 0;
        loop {
            let (looked, done, _) =
                self.fill_facts(&mut cursor, 256, Duration::ZERO, cancel, read, &|| ())?;
            recorded += done;
            if looked == 0 || cancel.load(Ordering::Relaxed) {
                return Ok(recorded);
            }
        }
    }
}

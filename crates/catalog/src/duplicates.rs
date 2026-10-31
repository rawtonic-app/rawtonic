//! Find Duplicates, stage 1: photographs whose files hold identical image
//! data (`image_hash`), whatever their names, folders, metadata or edits.
//! Each present photograph's hash is recorded with the file size and
//! modification time it was read at and counts only while the file still
//! has them (`image_hashes`, schema 20); `fill_image_hashes` brings
//! every stale or missing one up to date, `duplicate_groups` lists the
//! photographs sharing one. Nothing here moves, changes or removes a file:
//! the app trashes what the user marked through its journaled file
//! operations, after `confirm_duplicates` read the files again.
//!
//! Near-duplicates (visual similarity) and merging metadata are stage 2
//! (notes: parity-gaps/G36-organization.md).
use crate::Library;
use crate::image_hash::{current, image_hash};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::UNIX_EPOCH,
};

/// A file's size and modification time in nanoseconds: a recorded hash
/// holds while these do.
pub fn file_stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let nanos = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Some((meta.len(), i64::try_from(nanos).ok()?))
}

/// What a hash pass did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashPass {
    /// Present photographs looked at.
    pub photos: usize,
    /// Hashes read now (the rest were on record).
    pub hashed: usize,
    /// Files that could not be read (or changed while read): not grouped.
    pub unreadable: Vec<PathBuf>,
    pub cancelled: bool,
}

/// One photograph of a duplicate group, with what the review shows and the
/// suggestion weighs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateMember {
    pub id: String,
    pub path: PathBuf,
    pub size: u64,
    /// Effective capture time (`YYYY-MM-DD HH:MM:SS`), if known.
    pub captured: Option<String>,
    /// When it came into the library (UTC, SQLite form), if recorded.
    pub imported: Option<String>,
    pub rating: u8,
    pub flag: i8,
    pub label: String,
    pub keywords: usize,
    /// Virtual copies of it.
    pub copies: usize,
    /// It has a title, caption, creator, copyright or IPTC field.
    pub described: bool,
    /// Its RAW+JPEG companion, which goes to the Trash with it.
    pub companion: Option<PathBuf>,
    /// What part of the file was compared (`image_hash::Container` name):
    /// `file` means the whole file.
    pub compared: String,
}

/// Why a photograph marked for the Trash must stay (`confirm_duplicates`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// Its image data is no longer that of a kept photograph (its file, or
    /// the kept one's, changed or went).
    Changed,
    /// Its RAW+JPEG companion would go with it, and no kept photograph's
    /// companion holds the same image data.
    Companion,
}

/// A photograph to hash: its id, file and the stamp the file has now.
type Pending = (String, PathBuf, (u64, i64));

impl Library {
    /// Present, visible photographs whose image hash is not on record for
    /// their file as it is now, with the stamp it has now; and how many
    /// present photographs there are. Unreadable files are left out.
    fn image_hash_todo(&self) -> Result<(Vec<Pending>, usize)> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id,a.rel_path,r.path,r.external,h.hash,h.size,h.mtime
             FROM assets a JOIN roots r ON r.id=a.root_id LEFT JOIN image_hashes h ON h.asset_id=a.id
             WHERE a.missing=0 AND {} ORDER BY a.rowid",
            self.visible_assets()
        ))?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                self.row_path(r.get(1)?, r.get(2)?, r.get(3)?),
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut todo = Vec::new();
        let mut photos = 0;
        for row in rows {
            let (id, path, hash, size, mtime) = row?;
            photos += 1;
            let Some(stamp) = file_stamp(&path) else {
                continue;
            };
            let on_record = hash.as_deref().is_some_and(current)
                && size == i64::try_from(stamp.0).ok()
                && mtime == Some(stamp.1);
            if !on_record {
                todo.push((id, path, stamp));
            }
        }
        Ok((todo, photos))
    }

    fn row_path(&self, relative: String, root: String, external: bool) -> PathBuf {
        if external {
            PathBuf::from(root).join(relative)
        } else {
            self.directory.join(relative)
        }
    }

    /// How many present photographs have no current image hash.
    pub fn image_hash_backlog(&self) -> Result<usize> {
        Ok(self.image_hash_todo()?.0.len())
    }

    /// Records `hash` for asset `id` when its file still has `stamp` (the
    /// one taken before it was read). Returns whether it was recorded.
    pub fn record_image_hash(&self, id: &str, hash: &str, stamp: (u64, i64)) -> Result<bool> {
        let asset = self.asset(id)?;
        if file_stamp(&asset.path) != Some(stamp) {
            return Ok(false);
        }
        Ok(self.conn.execute(
            "INSERT INTO image_hashes(asset_id,hash,size,mtime) VALUES(?1,?2,?3,?4)
             ON CONFLICT(asset_id) DO UPDATE SET hash=?2,size=?3,mtime=?4",
            params![id, hash, i64::try_from(stamp.0)?, stamp.1],
        )? == 1)
    }

    /// Reads the image hash of every present photograph that has none on
    /// record for its file as it is now, `workers` files at a time, and
    /// records each. `progress(done, total)` follows the files read. Stops
    /// early, keeping what it recorded, when `cancel` is set.
    pub fn fill_image_hashes(
        &self,
        workers: usize,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<HashPass> {
        let (todo, photos) = self.image_hash_todo()?;
        let total = todo.len();
        let mut pass = HashPass {
            photos,
            ..Default::default()
        };
        progress(0, total);
        if total == 0 {
            return Ok(pass);
        }
        let (send, receive) = mpsc::channel();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| -> Result<()> {
            for _ in 0..workers.clamp(1, 16).min(total) {
                let send = send.clone();
                let (todo, next) = (&todo, &next);
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= todo.len() || cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let result = image_hash(&todo[i].1, cancel);
                        if send.send((i, result)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(send);
            let mut done = 0;
            for (i, result) in receive {
                let (id, path, stamp) = &todo[i];
                done += 1;
                match result {
                    Ok(Some(hash)) => {
                        if self.record_image_hash(id, &hash, *stamp)? {
                            pass.hashed += 1;
                        } else {
                            pass.unreadable.push(path.clone());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("Image data of {} not read: {e:#}", path.display());
                        pass.unreadable.push(path.clone());
                    }
                }
                progress(done, total);
            }
            Ok(())
        })?;
        pass.cancelled = cancel.load(Ordering::Relaxed);
        Ok(pass)
    }

    /// Groups of present, visible photographs with identical image data
    /// (current hashes only), each in import order (oldest first); groups
    /// ordered by their first photograph's path.
    pub fn duplicate_groups(&self) -> Result<Vec<Vec<DuplicateMember>>> {
        let visible = self.visible_assets();
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id,a.rel_path,r.path,r.external,a.size,{captured},
                    (SELECT b.created_at FROM asset_imports i JOIN import_batches b ON b.id=i.batch_id
                      WHERE i.asset_id=a.id),
                    m.rating,m.flag,m.label,m.data,
                    (SELECT count(*) FROM versions v WHERE v.asset_id=a.id AND v.is_primary=0),
                    h.hash,h.size,h.mtime,a.raw_jpeg_pair
             FROM assets a JOIN roots r ON r.id=a.root_id JOIN metadata m ON m.asset_id=a.id
             JOIN image_hashes h ON h.asset_id=a.id
             WHERE a.missing=0 AND {visible} AND h.hash IN (
               SELECT h.hash FROM image_hashes h JOIN assets a ON a.id=h.asset_id
                WHERE a.missing=0 AND {visible} GROUP BY h.hash HAVING count(*)>1)
             ORDER BY h.hash, a.rowid",
            captured = crate::capture_time::EFFECTIVE,
        ))?;
        let rows = query.query_map([], |r| {
            let data: String = r.get(10)?;
            let metadata: rawtonic_metadata::Metadata =
                serde_json::from_str(&data).unwrap_or_default();
            let described = [
                &metadata.title,
                &metadata.description,
                &metadata.creator,
                &metadata.copyright,
            ]
            .iter()
            .any(|text| !text.trim().is_empty())
                || metadata.iptc.values().any(|text| !text.trim().is_empty());
            let hash: String = r.get(12)?;
            let path = self.row_path(r.get(1)?, r.get(2)?, r.get(3)?);
            let companion = r
                .get::<_, Option<String>>(15)?
                .and_then(|name| path.parent().map(|folder| folder.join(name)))
                .filter(|companion| companion.is_file());
            Ok((
                hash.clone(),
                (r.get::<_, i64>(13)?, r.get::<_, i64>(14)?),
                DuplicateMember {
                    id: r.get(0)?,
                    path,
                    size: r.get::<_, i64>(4)? as u64,
                    captured: r.get(5)?,
                    imported: r.get(6)?,
                    rating: r.get(7)?,
                    flag: r.get(8)?,
                    label: r.get(9)?,
                    keywords: metadata.keywords.len(),
                    copies: r.get::<_, i64>(11)? as usize,
                    described,
                    companion,
                    compared: hash.split(':').nth(1).unwrap_or_default().to_string(),
                },
            ))
        })?;
        let mut groups: Vec<(String, Vec<DuplicateMember>)> = Vec::new();
        for row in rows {
            let (hash, (size, mtime), member) = row?;
            // A file changed since it was hashed is not grouped on old data.
            let Some(stamp) = file_stamp(&member.path) else {
                continue;
            };
            if !current(&hash) || i64::try_from(stamp.0)? != size || stamp.1 != mtime {
                continue;
            }
            match groups.last_mut() {
                Some((last, members)) if *last == hash => members.push(member),
                _ => groups.push((hash, vec![member])),
            }
        }
        let mut groups: Vec<Vec<DuplicateMember>> = groups
            .into_iter()
            .map(|(_, mut members)| {
                members.sort_by(|a, b| {
                    (a.imported.is_none(), &a.imported, &a.path).cmp(&(
                        b.imported.is_none(),
                        &b.imported,
                        &b.path,
                    ))
                });
                members
            })
            .filter(|members| members.len() > 1)
            .collect();
        groups.sort_by(|a, b| a[0].path.cmp(&b[0].path));
        Ok(groups)
    }

    /// Before photographs marked in a review are trashed: reads the image
    /// data of each group's photographs again (recorded hashes are used
    /// while their files are unchanged) and returns those of `trash` that
    /// must stay, with why (empty: all may go). A photograph goes only while
    /// a present photograph of `keep` holds the same image data, and, when
    /// it has a RAW+JPEG companion (trashed with it), while a kept
    /// photograph's companion holds that JPEG's image data.
    pub fn confirm_duplicates(
        &self,
        keep: &[PathBuf],
        trash: &[PathBuf],
    ) -> Result<Vec<(PathBuf, Refusal)>> {
        let mut kept = std::collections::HashSet::new();
        let mut kept_companions = std::collections::HashSet::new();
        for path in keep {
            if let Some(hash) = self.image_hash_now(path)? {
                kept.insert(hash);
            }
            if let Some(companion) = self.companion_at(path)?
                && let Some(hash) = image_hash(&companion, &AtomicBool::new(false))?
            {
                kept_companions.insert(hash);
            }
        }
        let mut refused = Vec::new();
        for path in trash {
            match self.image_hash_now(path)? {
                Some(hash) if kept.contains(&hash) => {}
                _ => {
                    refused.push((path.clone(), Refusal::Changed));
                    continue;
                }
            }
            if let Some(companion) = self.companion_at(path)? {
                match image_hash(&companion, &AtomicBool::new(false))? {
                    Some(hash) if kept_companions.contains(&hash) => {}
                    _ => refused.push((path.clone(), Refusal::Companion)),
                }
            }
        }
        Ok(refused)
    }

    /// The RAW+JPEG companion file of the photograph at `path`, if it has
    /// one and the file is there.
    fn companion_at(&self, path: &Path) -> Result<Option<PathBuf>> {
        let Some(asset) = self.asset_at(path)? else {
            return Ok(None);
        };
        let name: Option<String> = self.conn.query_row(
            "SELECT raw_jpeg_pair FROM assets WHERE id=?",
            [&asset.id],
            |r| r.get(0),
        )?;
        Ok(name
            .and_then(|name| asset.path.parent().map(|folder| folder.join(name)))
            .filter(|companion| companion.is_file()))
    }

    /// The image hash of the photograph at `path` as its file is now: the
    /// recorded one while the file is unchanged, else read (and recorded).
    /// `None` for a file that is not there or not a photograph of this
    /// library.
    fn image_hash_now(&self, path: &Path) -> Result<Option<String>> {
        let Some(asset) = self.asset_at(path)? else {
            return Ok(None);
        };
        let Some(stamp) = file_stamp(&asset.path) else {
            return Ok(None);
        };
        let (hash, size, mtime): (Option<String>, Option<i64>, Option<i64>) = self
            .conn
            .query_row(
                "SELECT hash,size,mtime FROM image_hashes WHERE asset_id=?",
                [&asset.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .unwrap_or((None, None, None));
        if let Some(hash) = hash.filter(|h| current(h))
            && size == i64::try_from(stamp.0).ok()
            && mtime == Some(stamp.1)
        {
            return Ok(Some(hash));
        }
        let Some(hash) = image_hash(&asset.path, &AtomicBool::new(false))? else {
            return Ok(None);
        };
        self.record_image_hash(&asset.id, &hash, stamp)?;
        Ok(Some(hash))
    }

    /// The image hashes on record by asset id (tests, the CLI).
    pub fn image_hashes(&self) -> Result<HashMap<String, Option<String>>> {
        Ok(self
            .conn
            .prepare(
                "SELECT a.id,h.hash FROM assets a LEFT JOIN image_hashes h ON h.asset_id=a.id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

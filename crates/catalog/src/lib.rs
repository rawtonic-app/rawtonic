//! Shared portable session/catalog storage, independent of Tauri and rendering.
pub mod portable;
use anyhow::{Context, Result, ensure};
use rawtonic_metadata::{Metadata, atomic_write};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

pub mod add;
pub mod checksum;
pub mod companion;
pub mod facts;
pub mod folder_sync;
pub mod folders;
pub mod hot_folder;
pub mod ingest_jobs;
pub mod query;
pub mod snapshots;
pub mod volume;
pub use add::{AddReport, Contents, ImportBatchInfo, ImportKind, PlannedAsset};
pub use facts::{CaptureFacts, Rated};
pub use hot_folder::{HotFolderConfig, HotFolderFile};
pub use query::{Combine, Field, Op, Rule};
pub use snapshots::Snapshot;
pub use volume::Volume;

/// Version of the session/catalog manifest (`session.json`).
pub const SCHEMA_VERSION: u32 = 1;
/// Version of the SQLite schema; older databases are migrated when opened.
pub const DB_SCHEMA_VERSION: u32 = 15;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Session,
    Catalog,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub mode: Mode,
    pub capture: String,
    pub output: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub rel_path: String,
    pub path: PathBuf,
    pub size: u64,
    pub fingerprint: String,
    pub rating: u8,
    pub label: String,
    pub flag: i8,
    pub missing: bool,
    /// The file's modification time (seconds) when it was registered.
    #[serde(default)]
    pub mtime: i64,
}

/// A session album (Capture One) / collection (Lightroom): a named set of
/// assets; removing it never touches the assets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub count: u32,
    /// "album" (holds photographs) or "group" (holds albums and groups).
    pub kind: String,
    /// Place among the albums of the same parent.
    pub position: i64,
    /// A smart album's rules (kind "smart"): which photographs it shows,
    /// evaluated whenever it is shown or counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Rule>,
}

pub const ALBUM: &str = "album";
pub const GROUP: &str = "group";
pub const SMART: &str = "smart";
/// The SQL function the search folds case with (`Library::configure`).
pub(crate) const FOLD: &str = "rawtonic_fold";

/// How the search folds case: Unicode lower case (`École` → `école`).
pub(crate) fn fold(text: &str) -> String {
    text.to_lowercase()
}

/// Lightweight startup read model. Recipe payloads stay in SQLite; opening a
/// library does not need to deserialize every existing photo's editor history.
pub struct AssetIndexEntry {
    pub id: String,
    pub needs_metadata_adoption: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub directory: PathBuf,
    pub manifest: Manifest,
    pub assets: usize,
    pub picked: usize,
    pub rated: usize,
    pub xmp_pending: usize,
    /// Photographs whose file is missing or on a disconnected drive; they
    /// stay in the library with their edits until relinked.
    #[serde(default)]
    pub missing: usize,
}

/// What `check_files` found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileCheck {
    /// Photographs whose file is missing now.
    pub missing: usize,
    /// Ids of photographs whose file is back (found again or followed).
    pub found: Vec<String>,
    /// (old place, new place) of photographs whose drive was followed to
    /// another mount point (their previews follow).
    pub followed: Vec<(PathBuf, PathBuf)>,
}

/// A registered photograph whose file cannot be found. It keeps its id,
/// edits, metadata and albums; nothing removes it automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OfflineAsset {
    pub id: String,
    /// Where the file was registered.
    pub path: PathBuf,
    pub size: u64,
    /// Its modification time (seconds) when it was registered.
    pub mtime: i64,
    /// The disconnected drive it is on (None: the drive is there and the
    /// file is gone, or the drive is not known).
    pub volume: Option<String>,
}

/// What a relink found and did. Nothing is moved or modified on disk; only
/// the library's record of where each photograph is changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RelinkReport {
    /// (registered place, new place) of every photograph found again.
    pub relinked: Vec<(PathBuf, PathBuf)>,
    /// Files with a missing photograph's name but other content (another
    /// photograph): left alone.
    pub mismatched: Vec<PathBuf>,
    /// Files that are already another photograph of this library.
    pub taken: Vec<PathBuf>,
    /// Missing photographs below the old folder with no file in the new one.
    pub not_found: usize,
}

pub struct Library {
    pub directory: PathBuf,
    pub manifest: Manifest,
    conn: Connection,
    /// The import batch new assets join (`Library::import_batch`): its kind
    /// and, once its first asset is in, its id.
    import: std::cell::Cell<Option<(ImportKind, Option<i64>)>>,
}

pub fn validate_relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Path must be relative and cannot traverse parents"
    );
    ensure!(
        !path.to_string_lossy().contains('\\'),
        "Backslashes are not portable path separators"
    );
    Ok(())
}

pub fn fingerprint(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let meta = file.metadata()?;
    ensure!(meta.is_file(), "Asset must be a regular file");
    let size = meta.len();
    let mut data = Vec::with_capacity(131_080);
    data.extend_from_slice(&size.to_le_bytes());
    (&mut file).take(65_536).read_to_end(&mut data)?;
    if size > 65_536 {
        file.seek(SeekFrom::Start(size.saturating_sub(65_536)))?;
        file.read_to_end(&mut data)?;
    }
    Ok(format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&data)))
}

fn db_name(mode: Mode) -> &'static str {
    if mode == Mode::Session {
        "session.sqlite"
    } else {
        "catalog.sqlite"
    }
}
fn manifest_name(mode: Mode) -> &'static str {
    if mode == Mode::Session {
        "session.json"
    } else {
        "catalog.json"
    }
}

impl Library {
    pub fn create(directory: &Path, name: &str, mode: Mode) -> Result<Self> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 255,
            "Library name must be 1–255 bytes"
        );
        fs::create_dir_all(directory)?;
        let directory = directory.canonicalize()?;
        ensure!(
            !directory.join("session.json").exists()
                && !directory.join("catalog.json").exists()
                && !directory.join(db_name(mode)).exists(),
            "A library already exists here; open it instead"
        );
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            mode,
            capture: if mode == Mode::Catalog {
                "Photos"
            } else {
                "Capture"
            }
            .into(),
            output: "Output".into(),
        };
        let conn = Connection::open(directory.join(db_name(mode)))?;
        Self::configure(&conn)?;
        conn.execute_batch(include_str!("../migrations/0001.sql"))?;
        conn.execute_batch(include_str!("../migrations/0002.sql"))?;
        conn.execute_batch(include_str!("../migrations/0003.sql"))?;
        conn.execute_batch(include_str!("../migrations/0004.sql"))?;
        conn.execute_batch(include_str!("../migrations/0005.sql"))?;
        conn.execute_batch(include_str!("../migrations/0006.sql"))?;
        conn.execute_batch(include_str!("../migrations/0007.sql"))?;
        conn.execute_batch(include_str!("../migrations/0008.sql"))?;
        conn.execute_batch(include_str!("../migrations/0009.sql"))?;
        conn.execute_batch(include_str!("../migrations/0010.sql"))?;
        conn.execute_batch(include_str!("../migrations/0011.sql"))?;
        conn.execute_batch(include_str!("../migrations/0012.sql"))?;
        conn.execute_batch(include_str!("../migrations/0013.sql"))?;
        conn.execute_batch(include_str!("../migrations/0014.sql"))?;
        conn.execute_batch(include_str!("../migrations/0015.sql"))?;
        conn.execute(
            "INSERT INTO roots(id,path,external) VALUES('session','.',0)",
            [],
        )?;
        for sub in [
            manifest.capture.as_str(),
            "Selects",
            "Output",
            "Trash",
            "masks",
            ".cache",
            "previews",
            "smart-previews",
        ] {
            if mode == Mode::Session && ["previews", "smart-previews"].contains(&sub) {
                continue;
            }
            fs::create_dir_all(directory.join(sub))?;
        }
        atomic_write(
            &directory.join(manifest_name(mode)),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(Self {
            directory,
            manifest,
            conn,
            import: Default::default(),
        })
    }

    pub fn open(directory: &Path) -> Result<Self> {
        let directory = directory.canonicalize()?;
        let manifest_path = if directory.join("session.json").exists() {
            directory.join("session.json")
        } else {
            directory.join("catalog.json")
        };
        let manifest: Manifest = serde_json::from_slice(
            &fs::read(&manifest_path).context("No Rawtonic session or catalog in this folder")?,
        )?;
        ensure!(
            manifest.schema_version == SCHEMA_VERSION,
            "Unsupported manifest version {}",
            manifest.schema_version
        );
        validate_relative(Path::new(&manifest.capture))?;
        validate_relative(Path::new(&manifest.output))?;
        let db = directory.join(db_name(manifest.mode));
        ensure!(
            db.is_file(),
            "Library database is missing; refusing to create an empty replacement"
        );
        ensure!(
            !fs::symlink_metadata(&db)?.file_type().is_symlink(),
            "Library database cannot be a symlink"
        );
        let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Ok(Self {
            directory,
            manifest,
            conn,
            import: Default::default(),
        })
    }

    /// Brings an older database up to `DB_SCHEMA_VERSION` in one immediate
    /// transaction (another connection may be migrating at the same time).
    /// Newer databases are refused rather than misread.
    fn migrate(conn: &Connection) -> Result<()> {
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            (1..=DB_SCHEMA_VERSION).contains(&version),
            "Unsupported database schema {version}; this Rawtonic reads up to {DB_SCHEMA_VERSION}"
        );
        if version == DB_SCHEMA_VERSION {
            return Ok(());
        }
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let version: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 2 {
            tx.execute_batch(include_str!("../migrations/0002.sql"))?;
        }
        if version < 3 {
            tx.execute_batch(include_str!("../migrations/0003.sql"))?;
        }
        if version < 4 {
            tx.execute_batch(include_str!("../migrations/0004.sql"))?;
        }
        if version < 5 {
            tx.execute_batch(include_str!("../migrations/0005.sql"))?;
        }
        if version < 6 {
            tx.execute_batch(include_str!("../migrations/0006.sql"))?;
        }
        if version < 7 {
            tx.execute_batch(include_str!("../migrations/0007.sql"))?;
        }
        if version < 8 {
            tx.execute_batch(include_str!("../migrations/0008.sql"))?;
        }
        if version < 9 {
            tx.execute_batch(include_str!("../migrations/0009.sql"))?;
        }
        if version < 10 {
            tx.execute_batch(include_str!("../migrations/0010.sql"))?;
        }
        if version < 11 {
            tx.execute_batch(include_str!("../migrations/0011.sql"))?;
        }
        if version < 12 {
            tx.execute_batch(include_str!("../migrations/0012.sql"))?;
        }
        if version < 13 {
            tx.execute_batch(include_str!("../migrations/0013.sql"))?;
        }
        if version < 14 {
            tx.execute_batch(include_str!("../migrations/0014.sql"))?;
        }
        if version < 15 {
            tx.execute_batch(include_str!("../migrations/0015.sql"))?;
        }
        tx.commit()?;
        Ok(())
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        // The search's case folding (`query`, `fold`): the text index holds
        // folded text (its triggers call it), and every search that scans
        // rows folds with it too, so both find the same.
        conn.create_scalar_function(
            FOLD,
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                Ok(match ctx.get_raw(0) {
                    rusqlite::types::ValueRef::Text(text) => {
                        Some(fold(&String::from_utf8_lossy(text)))
                    }
                    rusqlite::types::ValueRef::Integer(n) => Some(n.to_string()),
                    rusqlite::types::ValueRef::Real(n) => Some(n.to_string()),
                    _ => None,
                })
            },
        )?;
        // Keywords are indexed trimmed as rules trim their values (Unicode
        // whitespace, `str::trim`), by the keyword triggers.
        conn.create_scalar_function(
            "rawtonic_trim",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                Ok(match ctx.get_raw(0) {
                    rusqlite::types::ValueRef::Text(text) => {
                        Some(String::from_utf8_lossy(text).trim().to_string())
                    }
                    _ => None,
                })
            },
        )?;
        Ok(())
    }

    pub fn summary(&self) -> Result<Summary> {
        let (assets,picked,rated,pending) = self.conn.query_row("SELECT count(*),coalesce(sum(flag=1),0),coalesce(sum(rating>0),0),coalesce(sum(xmp_status!='synced'),0) FROM metadata", [], |r| Ok((r.get::<_,i64>(0)? as usize,r.get::<_,i64>(1)? as usize,r.get::<_,i64>(2)? as usize,r.get::<_,i64>(3)? as usize)))?;
        let missing: i64 =
            self.conn
                .query_row("SELECT count(*) FROM assets WHERE missing=1", [], |r| {
                    r.get(0)
                })?;
        Ok(Summary {
            directory: self.directory.clone(),
            manifest: self.manifest.clone(),
            assets,
            picked,
            rated,
            xmp_pending: pending,
            missing: missing as usize,
        })
    }

    /// Registers one file without reading its sidecars (an already
    /// registered file returns its asset). Sessions only accept contained
    /// files; catalogs register an external root. `add_files` also brings
    /// each file's sidecar edits and XMP metadata, in the same transaction.
    pub fn add_asset(&self, path: &Path) -> Result<Asset> {
        let planned = self.plan_add(path, Contents::default())?;
        if let Some(id) = self.registered_id(&planned.root, &planned.rel)? {
            return self.asset(&id);
        }
        let (id, _) = self
            .insert_planned(vec![planned])?
            .pop()
            .context("Nothing was registered")?;
        self.asset(&id)
    }

    pub fn asset(&self, id: &str) -> Result<Asset> {
        self.conn.query_row("SELECT a.id,a.rel_path,a.size,a.fingerprint,m.rating,m.label,m.flag,a.missing,r.path,r.external,a.mtime FROM assets a JOIN metadata m ON m.asset_id=a.id JOIN roots r ON r.id=a.root_id WHERE a.id=?", [id], |r| {
            let relative: String = r.get(1)?; let root: String = r.get(8)?; let external: bool = r.get(9)?;
            Ok(Asset { id:r.get(0)?, path: if external { PathBuf::from(root).join(&relative) } else { self.directory.join(&relative) }, rel_path:relative, size:r.get::<_,i64>(2)? as u64, fingerprint:r.get(3)?, rating:r.get(4)?, label:r.get(5)?, flag:r.get(6)?, missing:r.get(7)?, mtime:r.get(10)? })
        }).map_err(Into::into)
    }

    /// The asset registered at `path`; also when its file is missing (the
    /// path is resolved through its nearest existing folder).
    pub fn asset_at(&self, path: &Path) -> Result<Option<Asset>> {
        let path = resolve_moved(path);
        let id = if let Ok(relative) = path.strip_prefix(&self.directory) {
            self.conn
                .query_row(
                    "SELECT id FROM assets WHERE root_id='session' AND rel_path=?",
                    [relative.to_string_lossy().as_ref()],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
        } else {
            self.conn.query_row("SELECT a.id FROM assets a JOIN roots r ON a.root_id=r.id WHERE r.external=1 AND r.path=? AND a.rel_path=?", params![path.parent().context("No parent")?.to_string_lossy().as_ref(),path.file_name().context("No filename")?.to_string_lossy().as_ref()], |r| r.get::<_, String>(0)).optional()?
        };
        id.map(|id| self.asset(&id)).transpose()
    }

    /// Every asset, or those whose searchable text holds every word of
    /// `query` (the Library's search box, `Rule::text`).
    pub fn assets(&self, query: Option<&str>) -> Result<Vec<Asset>> {
        let ids: Vec<String> = if let Some(q) = query.filter(|s| !s.trim().is_empty()) {
            self.search_ids(&Rule::text(q))?
        } else {
            self.conn
                .prepare("SELECT id FROM assets ORDER BY rel_path")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.asset(id)).collect()
    }

    pub fn metadata(&self, id: &str) -> Result<Metadata> {
        let data: String =
            self.conn
                .query_row("SELECT data FROM metadata WHERE asset_id=?", [id], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Empty schema defaults have never been adopted or edited. Even an explicit
    /// clear-to-zero is initialized metadata and must not be reimported on open.
    pub fn metadata_initialized(&self, id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT data != '{}' FROM metadata WHERE asset_id=?",
            [id],
            |row| row.get(0),
        )?)
    }

    pub fn index_entries(&self) -> Result<HashMap<PathBuf, AssetIndexEntry>> {
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external,
                    CASE WHEN m.data != '{}' AND m.from_camera=0 THEN 0
                         ELSE p.payload IS NULL OR json_type(p.payload,'$.editor_state') IS NULL END
             FROM assets a JOIN roots r ON r.id=a.root_id
             JOIN metadata m ON m.asset_id=a.id
             JOIN versions v ON v.asset_id=a.id AND v.is_primary=1
             LEFT JOIN recipes p ON p.id=v.head",
        )?;
        let rows = query.query_map([], |row| {
            let relative: String = row.get(1)?;
            let root: String = row.get(2)?;
            let external: bool = row.get(3)?;
            let path = if external {
                PathBuf::from(root).join(relative)
            } else {
                self.directory.join(relative)
            };
            Ok((
                path,
                AssetIndexEntry {
                    id: row.get(0)?,
                    needs_metadata_adoption: row.get(4)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sparse culling read model, resolved against the current library location.
    /// One query; no per-photo database opens or original-file reads.
    pub fn flags(&self) -> Result<std::collections::HashMap<PathBuf, i8>> {
        let mut query = self.conn.prepare(
            "SELECT a.rel_path,r.path,r.external,m.flag FROM assets a
             JOIN roots r ON r.id=a.root_id JOIN metadata m ON m.asset_id=a.id
             WHERE m.flag != 0",
        )?;
        let rows = query.query_map([], |row| {
            let relative: String = row.get(0)?;
            let root: String = row.get(1)?;
            let external: bool = row.get(2)?;
            let path = if external {
                PathBuf::from(root).join(relative)
            } else {
                self.directory.join(relative)
            };
            Ok((path, row.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Commit an entire selection together, preserving ratings, keywords and edits.
    pub fn set_flags(&self, ids: &[String], flag: i8) -> Result<()> {
        ensure!((-1..=1).contains(&flag), "Flag must be -1, 0 or 1");
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            let mut metadata = self.metadata(id)?;
            metadata.flag = flag;
            tx.execute(
                "UPDATE metadata SET flag=?,data=?,xmp_status='pending',from_camera=0 WHERE asset_id=?",
                params![flag, serde_json::to_string(&metadata)?, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_metadata(&self, id: &str, metadata: &Metadata) -> Result<()> {
        metadata.validate()?;
        let tx = self.conn.unchecked_transaction()?;
        write_metadata(&tx, id, metadata)?;
        tx.commit()?;
        Ok(())
    }

    /// Every asset's path and metadata in one query (the Library's metadata
    /// tools cache it per session).
    pub fn all_metadata(&self) -> Result<Vec<(PathBuf, Metadata)>> {
        let mut query = self.conn.prepare(
            "SELECT a.rel_path,r.path,r.external,m.data FROM assets a \
             JOIN metadata m ON m.asset_id=a.id JOIN roots r ON r.id=a.root_id",
        )?;
        let rows = query.query_map([], |r| {
            let relative: String = r.get(0)?;
            let root: String = r.get(1)?;
            let external: bool = r.get(2)?;
            let data: String = r.get(3)?;
            let path = if external {
                PathBuf::from(root).join(&relative)
            } else {
                self.directory.join(&relative)
            };
            Ok((path, data))
        })?;
        rows.map(|row| {
            let (path, data) = row?;
            Ok((path, serde_json::from_str(&data).unwrap_or_default()))
        })
        .collect()
    }

    /// Albums by name, with how many assets each holds (a smart album:
    /// how many its rules match now; damaged rules: none).
    pub fn collections(&self) -> Result<Vec<Collection>> {
        let mut query = self.conn.prepare(
            "SELECT c.id,c.name,c.parent,(SELECT COUNT(*) FROM collection_assets ca WHERE ca.collection_id=c.id),c.kind,c.position,c.rules \
             FROM collections c ORDER BY c.position, c.name COLLATE NOCASE, c.id",
        )?;
        let rows = query.query_map([], |r| {
            let rules: Option<String> = r.get(6)?;
            Ok(Collection {
                id: r.get(0)?,
                name: r.get(1)?,
                parent: r.get(2)?,
                count: r.get(3)?,
                kind: r.get(4)?,
                position: r.get(5)?,
                rules: rules.and_then(|rules| serde_json::from_str(&rules).ok()),
            })
        })?;
        let mut collections = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for album in &mut collections {
            if album.kind == SMART {
                album.count = self.search_count(&Rule::album(&album.id)).unwrap_or(0) as u32;
            }
        }
        Ok(collections)
    }

    /// A new smart album with `rules` at the end of `parent` (a group or
    /// the top level). Folders inside the library are kept relative to it
    /// (`Rule::relative_to`), so the album moves with a session.
    pub fn create_smart_collection(
        &self,
        name: &str,
        parent: Option<&str>,
        rules: &Rule,
    ) -> Result<Collection> {
        let name = Self::collection_name(name)?;
        let rules = &rules.relative_to(&self.directory);
        rules.validate()?;
        self.ensure_group(parent)?;
        let id = Uuid::new_v4().to_string();
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1,0) FROM collections WHERE parent IS ?",
            [parent],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO collections(id,name,parent,kind,position,rules) VALUES(?,?,?,?,?,?)",
            params![
                id,
                name,
                parent,
                SMART,
                position,
                serde_json::to_string(rules)?
            ],
        )?;
        Ok(Collection {
            id: id.clone(),
            name,
            parent: parent.map(str::to_string),
            count: self.search_count(&Rule::album(&id))? as u32,
            kind: SMART.to_string(),
            position,
            rules: Some(rules.clone()),
        })
    }

    /// Replaces a smart album's rules (folders inside the library kept
    /// relative to it, as `create_smart_collection` does).
    pub fn set_collection_rules(&self, id: &str, rules: &Rule) -> Result<()> {
        let rules = &rules.relative_to(&self.directory);
        rules.validate()?;
        ensure!(
            self.conn.execute(
                "UPDATE collections SET rules=? WHERE id=? AND kind=?",
                params![serde_json::to_string(rules)?, id, SMART]
            )? == 1,
            "Unknown smart album"
        );
        Ok(())
    }

    fn collection_name(name: &str) -> Result<String> {
        let name = name.trim();
        ensure!(
            !name.is_empty() && name.len() <= 255,
            "Album names must be 1–255 bytes"
        );
        ensure!(
            !name.chars().any(char::is_control),
            "Album names cannot contain control characters"
        );
        Ok(name.to_string())
    }

    pub fn create_collection(&self, name: &str, parent: Option<&str>) -> Result<Collection> {
        self.create_collection_of_kind(name, parent, ALBUM)
    }

    /// A new album or group at the end of its parent, which must be a group.
    pub fn create_collection_of_kind(
        &self,
        name: &str,
        parent: Option<&str>,
        kind: &str,
    ) -> Result<Collection> {
        let name = Self::collection_name(name)?;
        ensure!(
            kind == ALBUM || kind == GROUP,
            "Unknown album kind (smart albums are made with their rules)"
        );
        self.ensure_group(parent)?;
        let id = Uuid::new_v4().to_string();
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1,0) FROM collections WHERE parent IS ?",
            [parent],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO collections(id,name,parent,kind,position) VALUES(?,?,?,?,?)",
            params![id, name, parent, kind, position],
        )?;
        Ok(Collection {
            id,
            name,
            parent: parent.map(str::to_string),
            count: 0,
            kind: kind.to_string(),
            position,
            rules: None,
        })
    }

    /// `parent` is the top level or an existing group.
    fn ensure_group(&self, parent: Option<&str>) -> Result<()> {
        if let Some(parent) = parent {
            let kind: Option<String> = self
                .conn
                .query_row("SELECT kind FROM collections WHERE id=?", [parent], |r| {
                    r.get(0)
                })
                .optional()?;
            ensure!(kind.is_some(), "Unknown parent album");
            ensure!(
                kind.as_deref() == Some(GROUP),
                "Only groups can contain albums"
            );
        }
        Ok(())
    }

    /// Moves an album or group into `parent` (top level when `None`) at
    /// `index` among its siblings; a group cannot move into itself.
    pub fn move_collection(&self, id: &str, parent: Option<&str>, index: usize) -> Result<()> {
        self.ensure_group(parent)?;
        let tx = self.conn.unchecked_transaction()?;
        ensure!(
            tx.query_row("SELECT COUNT(*) FROM collections WHERE id=?", [id], |r| r
                .get::<_, i64>(
                0
            ))? == 1,
            "Unknown album"
        );
        if let Some(parent) = parent {
            let inside: bool = tx.query_row(
                "WITH RECURSIVE up(id,parent) AS (SELECT id,parent FROM collections WHERE id=? \
                 UNION SELECT c.id,c.parent FROM collections c JOIN up ON c.id=up.parent) \
                 SELECT EXISTS(SELECT 1 FROM up WHERE id=?)",
                params![parent, id],
                |r| r.get(0),
            )?;
            ensure!(!inside, "A group cannot move into itself");
        }
        let mut siblings: Vec<String> = tx
            .prepare(
                "SELECT id FROM collections WHERE parent IS ? AND id<>? \
                 ORDER BY position, name COLLATE NOCASE, id",
            )?
            .query_map(params![parent, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        siblings.insert(index.min(siblings.len()), id.to_string());
        tx.execute(
            "UPDATE collections SET parent=? WHERE id=?",
            params![parent, id],
        )?;
        for (position, sibling) in siblings.iter().enumerate() {
            tx.execute(
                "UPDATE collections SET position=? WHERE id=?",
                params![position as i64, sibling],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn rename_collection(&self, id: &str, name: &str) -> Result<()> {
        let name = Self::collection_name(name)?;
        ensure!(
            self.conn.execute(
                "UPDATE collections SET name=? WHERE id=?",
                params![name, id]
            )? == 1,
            "Unknown album"
        );
        Ok(())
    }

    /// Removes an album and the albums inside it; their assets stay.
    pub fn delete_collection(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let ids: Vec<String> = tx
            .prepare(
                "WITH RECURSIVE tree(id) AS (SELECT id FROM collections WHERE id=? \
                 UNION SELECT c.id FROM collections c JOIN tree t ON c.parent=t.id) SELECT id FROM tree",
            )?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(!ids.is_empty(), "Unknown album");
        // Children first: their parent reference must not dangle.
        for id in ids.iter().rev() {
            tx.execute("DELETE FROM collections WHERE id=?", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Adds assets (already in the album: kept once). All or nothing.
    pub fn add_to_collection(&self, id: &str, assets: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let kind: Option<String> = tx
            .query_row("SELECT kind FROM collections WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(kind.is_some(), "Unknown album");
        ensure!(
            kind.as_deref() == Some(ALBUM),
            "Only albums hold photographs (groups hold albums, smart albums rules)"
        );
        for asset in assets {
            tx.execute(
                "INSERT OR IGNORE INTO collection_assets(collection_id,asset_id) VALUES(?,?)",
                params![id, asset],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_from_collection(&self, id: &str, assets: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let kind: Option<String> = tx
            .query_row("SELECT kind FROM collections WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(
            kind.as_deref() != Some(SMART),
            "A smart album shows the photographs its rules match; change its rules to leave them out"
        );
        for asset in assets {
            tx.execute(
                "DELETE FROM collection_assets WHERE collection_id=? AND asset_id=?",
                params![id, asset],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The album's assets in path order; a smart album's are those its
    /// rules match now, a group's those of the albums in it.
    pub fn collection_assets(&self, id: &str) -> Result<Vec<Asset>> {
        let ids = self.search_ids(&Rule::album(id))?;
        ids.iter().map(|id| self.asset(id)).collect()
    }

    /// Paths of the album's photographs (as `collection_assets`), in one
    /// query.
    pub fn collection_paths(&self, id: &str) -> Result<Vec<PathBuf>> {
        self.search(&Rule::album(id))
    }

    pub fn primary_version(&self, asset: &str) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT id FROM versions WHERE asset_id=? AND is_primary=1",
            [asset],
            |r| r.get(0),
        )?)
    }

    pub fn save_recipe(
        &self,
        version: &str,
        payload: &Value,
        snapshot: Option<&str>,
    ) -> Result<i64> {
        ensure!(
            payload.get("schema_version").and_then(Value::as_u64) == Some(1),
            "Unsupported recipe schema"
        );
        ensure!(
            payload
                .get("layers")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty),
            "Independent layers are not implemented; use the inherited mask recipe"
        );
        ensure!(
            serde_json::to_string(payload)?.len() <= 64 * 1024 * 1024,
            "Recipe too large"
        );
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut stored = payload.clone();
        let hashes = store_blobs(&tx, &mut stored)?;
        let bytes = serde_json::to_string(&stored)?;
        let head: Option<i64> =
            tx.query_row("SELECT head FROM versions WHERE id=?", [version], |r| {
                r.get(0)
            })?;
        if let Some(head) = head {
            let old: String =
                tx.query_row("SELECT payload FROM recipes WHERE id=?", [head], |r| {
                    r.get(0)
                })?;
            if old == bytes && snapshot.is_none() {
                tx.commit()?;
                return Ok(head);
            }
        }
        tx.execute(
            "INSERT INTO recipes(version_id,parent,payload,snapshot) VALUES(?,?,?,?)",
            params![version, head, bytes, snapshot],
        )?;
        let id = tx.last_insert_rowid();
        refer_blobs(&tx, id, &hashes)?;
        tx.execute(
            "UPDATE versions SET head=? WHERE id=?",
            params![id, version],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn recipe(&self, version: &str) -> Result<Option<Value>> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT r.payload FROM versions v JOIN recipes r ON r.id=v.head WHERE v.id=?",
                [version],
                |r| r.get(0),
            )
            .optional()?;
        text.map(|s| {
            let mut value = serde_json::from_str(&s)?;
            load_blobs(&self.conn, &mut value)?;
            Ok(value)
        })
        .transpose()
    }

    /// Editor adapter: the DB owns the serialized state; sidecars are derived.
    /// The snapshots `legacy` carries are the library's own (they are not
    /// stored from it), except at the first save of a photograph whose
    /// sidecar the library never adopted (`is_untouched`; one the editor
    /// loaded from that sidecar): then they are taken over, in the same
    /// transaction.
    pub fn save_editor_state(&self, asset: &str, legacy: &Value) -> Result<()> {
        let version = self.primary_version(asset)?;
        let (bytes, meta) = self.editor_recipe(legacy, self.metadata(asset)?)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let adopting = add::untouched(&tx, asset)? && !snapshots::any(&tx, &version)?;
        write_recipe(&tx, &version, &bytes)?;
        write_metadata(&tx, asset, &meta)?;
        if adopting {
            snapshots::insert(&tx, &self.directory, &version, &snapshots::in_state(legacy))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The recipe payload an editor state is stored as, and the metadata it
    /// means on top of `meta` (its rating, keywords and colour label).
    /// Reads nothing but `legacy`; validated before anything is written.
    fn editor_recipe(&self, legacy: &Value, mut meta: Metadata) -> Result<(String, Metadata)> {
        let mut stored = legacy.clone();
        // Snapshots are kept in their own table, not in every history step.
        snapshots::strip(&mut stored);
        rawtonic_metadata::resources::localize(&mut stored, &self.directory);
        let legacy = &stored;
        let payload = json!({"schema_version":1,"process_version":"rapidraw-1.6.4","adjustments":legacy.get("adjustments").cloned().unwrap_or(Value::Null),"editor_state":legacy,"layers":[]});
        if let Some(rating) = legacy.get("rating").and_then(Value::as_u64) {
            ensure!(rating <= 5, "Rating must be 0–5");
            meta.rating = rating as u8;
        }
        if let Some(tags) = legacy.get("tags") {
            meta.keywords.clear();
            meta.label.clear();
            for tag in tags
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(color) = tag.strip_prefix("color:") {
                    let mut chars = color.chars();
                    meta.label = chars
                        .next()
                        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                        .unwrap_or_default();
                } else {
                    // The editor marks keywords the user typed with `user:`;
                    // the catalog and XMP store the keyword itself.
                    let keyword = tag.strip_prefix(USER_TAG_PREFIX).unwrap_or(tag);
                    if !meta.keywords.iter().any(|k| k == keyword) {
                        meta.keywords.push(keyword.to_string());
                    }
                }
            }
        }
        // The editor changes labels through tags. A loaded state can still carry
        // the previous adapter-only colorLabel value; it must not undo that edit.
        if legacy.get("tags").is_none()
            && let Some(label) = legacy
                .pointer("/adjustments/colorLabel")
                .and_then(Value::as_str)
        {
            meta.label = label.into();
        }
        meta.validate()?;
        let bytes = serde_json::to_string(&payload)?;
        ensure!(bytes.len() <= 64 * 1024 * 1024, "Recipe too large");
        Ok((bytes, meta))
    }

    /// The editor state of `asset`'s primary version, with its snapshots.
    pub fn editor_state(&self, asset: &str) -> Result<Option<Value>> {
        let version = self.primary_version(asset)?;
        let snapshots = self.snapshots(&version)?;
        let state = self
            .recipe(&version)?
            .and_then(|r| r.get("editor_state").cloned())
            // Snapshots of a photograph never edited: a state to carry them.
            .or_else(|| {
                (!snapshots.is_empty())
                    .then(|| json!({"version": 1, "rating": 0, "tags": [], "adjustments": null}))
            });
        let mut state = with_library_metadata(state, self.metadata(asset)?);
        if let Some(ref mut state) = state {
            rawtonic_metadata::resources::resolve(state, &self.directory);
            if !snapshots.is_empty() {
                snapshots::set_in_state(state, &snapshots)?;
            }
        }
        Ok(state)
    }

    /// One connection/query for a listing. Stream one editor payload at a time
    /// to let callers retain only their compact browser fields. Primary heads
    /// and metadata overrides have the same meaning as `editor_state`.
    pub fn visit_editor_states(
        &self,
        paths: &HashSet<PathBuf>,
        mut visit: impl FnMut(&Path, Option<Value>),
    ) -> Result<()> {
        let mut query = self.conn.prepare(
            "SELECT a.rel_path,r.path,r.external,p.payload,m.data
             FROM assets a JOIN roots r ON r.id=a.root_id
             JOIN metadata m ON m.asset_id=a.id
             JOIN versions v ON v.asset_id=a.id AND v.is_primary=1
             LEFT JOIN recipes p ON p.id=v.head",
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let relative: String = row.get(0)?;
            let root: String = row.get(1)?;
            let external: bool = row.get(2)?;
            let path = if external {
                PathBuf::from(root).join(relative)
            } else {
                self.directory.join(relative)
            };
            if !paths.contains(&path) {
                continue;
            }
            let payload: Option<String> = row.get(3)?;
            let mut state = payload
                .map(|text| serde_json::from_str::<Value>(&text))
                .transpose()?
                .and_then(|recipe| recipe.get("editor_state").cloned());
            if let Some(ref mut state) = state {
                load_blobs(&self.conn, state)?;
            }
            let metadata: String = row.get(4)?;
            let mut state = with_library_metadata(state, serde_json::from_str(&metadata)?);
            if let Some(ref mut state) = state {
                rawtonic_metadata::resources::resolve(state, &self.directory);
            }
            visit(&path, state);
        }
        Ok(())
    }

    /// Adopt an existing sidecar only as an explicit initialization/sync action.
    pub fn adopt_xmp(&self, asset: &str) -> Result<Metadata> {
        let path = self.asset(asset)?.path.with_extension("xmp");
        let text = fs::read_to_string(path)?;
        let (metadata, _) = rawtonic_metadata::read_xmp(&text)?;
        self.set_metadata(asset, &metadata)?;
        self.conn.execute(
            "UPDATE metadata SET xmp_hash=?,xmp_status='synced' WHERE asset_id=?",
            params![rawtonic_metadata::digest(text.as_bytes()), asset],
        )?;
        Ok(metadata)
    }

    pub fn sync_xmp(&self, asset: &str) -> Result<()> {
        let a = self.asset(asset)?;
        // RAW+JPEG siblings would share a stem; refuse ambiguity rather than clobber.
        let same_stem = self
            .assets(None)?
            .iter()
            .filter(|other| other.path.with_extension("xmp") == a.path.with_extension("xmp"))
            .count();
        ensure!(
            same_stem == 1,
            "RAW/JPEG sidecar ownership needs pair resolution"
        );
        let hash: Option<String> = self.conn.query_row(
            "SELECT xmp_hash FROM metadata WHERE asset_id=?",
            [asset],
            |r| r.get(0),
        )?;
        let recipe = self.recipe(&self.primary_version(asset)?)?;
        if hash.is_none() && a.path.with_extension("xmp").exists() {
            self.conn.execute(
                "UPDATE metadata SET xmp_status='conflict' WHERE asset_id=?",
                [asset],
            )?;
            anyhow::bail!("Existing XMP has not been adopted; refusing to replace its metadata");
        }
        match rawtonic_metadata::write_xmp(
            &a.path.with_extension("xmp"),
            &self.metadata(asset)?,
            recipe.as_ref(),
            hash.as_deref(),
        ) {
            Ok(hash) => {
                self.conn.execute(
                    "UPDATE metadata SET xmp_hash=?,xmp_status='synced' WHERE asset_id=?",
                    params![hash, asset],
                )?;
                Ok(())
            }
            Err(e) => {
                self.conn.execute(
                    "UPDATE metadata SET xmp_status='conflict' WHERE asset_id=?",
                    [asset],
                )?;
                Err(e)
            }
        }
    }

    pub fn undo(&self, version: &str) -> Result<bool> {
        let parent: Option<i64> = self
            .conn
            .query_row(
                "SELECT r.parent FROM versions v JOIN recipes r ON r.id=v.head WHERE v.id=?",
                [version],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(parent) = parent {
            self.conn.execute(
                "UPDATE versions SET head=? WHERE id=?",
                params![parent, version],
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Marks registered photographs whose file is gone as missing and found
    /// ones as present again; never removes any. Catalog folders on a drive
    /// that is now mounted elsewhere (another mount point or name) are
    /// followed first (`follow_volumes`). Returns how many are missing.
    pub fn verify_paths(&self) -> Result<usize> {
        Ok(self.check_files()?.missing)
    }

    /// `verify_paths`, also telling which photographs came back (their file
    /// was missing and is found again, or their drive was followed), so
    /// files derived from the library can be brought up to date.
    pub fn check_files(&self) -> Result<FileCheck> {
        let mut found = Vec::new();
        let mut followed = Vec::new();
        if self.has_lost_roots()? {
            for (id, from, to) in self.follow_volume_ids(&volume::mounted_volumes())? {
                found.push(id);
                followed.push((from, to));
            }
        }
        // Verify only registered paths. A single joined read replaces the
        // former full Asset materialization and one SELECT per photo.
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external,a.missing
             FROM assets a JOIN roots r ON r.id=a.root_id",
        )?;
        let rows = query.query_map([], |row| {
            let path = self.join_root(row.get(2)?, row.get(3)?, row.get::<_, String>(1)?);
            Ok((row.get::<_, String>(0)?, path, row.get::<_, bool>(4)?))
        })?;
        let mut missing = 0;
        let mut changes = Vec::new();
        for row in rows {
            let (id, path, was_missing) = row?;
            let is_missing = !path.is_file();
            missing += usize::from(is_missing);
            if was_missing != is_missing {
                if !is_missing {
                    found.push(id.clone());
                }
                changes.push((id, is_missing));
            }
        }
        drop(query);
        // An unchanged reopen writes nothing. Missing/restored files commit
        // together rather than forcing a FULL-synchronous commit per asset.
        if !changes.is_empty() {
            let tx = self.conn.unchecked_transaction()?;
            {
                let mut update = tx.prepare("UPDATE assets SET missing=? WHERE id=?")?;
                for (id, is_missing) in changes {
                    update.execute(params![is_missing, id])?;
                }
            }
            tx.commit()?;
        }
        Ok(FileCheck {
            missing,
            found,
            followed,
        })
    }

    /// The absolute path of `relative` in a root.
    fn join_root(&self, root: String, external: bool, relative: String) -> PathBuf {
        if external {
            PathBuf::from(root).join(relative)
        } else {
            self.directory.join(relative)
        }
    }

    /// Whether an external root's folder is gone (a drive disconnected,
    /// renamed or mounted elsewhere).
    fn has_lost_roots(&self) -> Result<bool> {
        let mut query = self
            .conn
            .prepare("SELECT path FROM roots WHERE external=1 AND volume_id IS NOT NULL")?;
        let paths: Vec<String> = query
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(paths.iter().any(|path| !Path::new(path).is_dir()))
    }

    /// Catalog folders whose drive is mounted under another mount point
    /// (`/Volumes/Photos 1` after a name clash, a renamed drive, another
    /// Linux mount) are followed there: photographs whose file is at the
    /// same place on that drive, with the registered size, move to it.
    /// Returns how many moved.
    pub fn follow_volumes(&self, mounted: &[Volume]) -> Result<usize> {
        Ok(self.follow_volume_ids(mounted)?.len())
    }

    fn follow_volume_ids(&self, mounted: &[Volume]) -> Result<Vec<(String, PathBuf, PathBuf)>> {
        let roots: Vec<(String, String, String, String)> = self
            .conn
            .prepare(
                "SELECT id,path,volume_id,volume_path FROM roots
                 WHERE external=1 AND volume_id IS NOT NULL AND volume_path IS NOT NULL",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut moved = Vec::new();
        for (root, path, volume, mount) in roots {
            let mount = mount.as_str();
            let folder = Path::new(&path);
            if folder.is_dir() {
                continue;
            }
            let Ok(inside) = folder.strip_prefix(mount) else {
                continue;
            };
            let Some(now) = mounted
                .iter()
                .find(|v| v.matches_id(&volume) && v.mount_point != Path::new(mount))
            else {
                continue;
            };
            let target = now.mount_point.join(inside);
            if !target.is_dir() {
                continue;
            }
            let mut query = self
                .conn
                .prepare("SELECT id,rel_path,size FROM assets WHERE root_id=?")?;
            let assets = query.query_map([&root], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for asset in assets {
                let (id, relative, size) = asset?;
                let file = target.join(&relative);
                if fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() as i64 == size) {
                    moved.push((id, file, folder.join(&relative)));
                }
            }
        }
        if moved.is_empty() {
            return Ok(Vec::new());
        }
        let places: Vec<(String, PathBuf)> = moved
            .iter()
            .map(|(id, file, _)| (id.clone(), file.clone()))
            .collect();
        let tx = self.conn.unchecked_transaction()?;
        self.apply_places(&tx, &places)?;
        tx.commit()?;
        Ok(moved
            .into_iter()
            .map(|(id, to, from)| (id, from, to))
            .collect())
    }

    /// The root, path in the root and root folder for a file at `path`:
    /// inside the library folder its session root; elsewhere (catalogs only)
    /// an external root for its folder. None for a session file outside.
    fn place_for(&self, path: &Path) -> Result<Option<(String, PathBuf, String)>> {
        Ok(if let Ok(relative) = path.strip_prefix(&self.directory) {
            Some((
                "session".to_string(),
                relative.to_path_buf(),
                ".".to_string(),
            ))
        } else if self.manifest.mode == Mode::Catalog {
            let parent = path.parent().context("No parent")?;
            Some((
                external_root_key(parent),
                PathBuf::from(path.file_name().context("No filename")?),
                parent.to_string_lossy().into_owned(),
            ))
        } else {
            None
        })
    }

    /// Where an asset moved to `path` is recorded: as `place_for`, and for a
    /// session photograph that left the session folder (moved by the app to
    /// another drive or folder, ADR 0016) an external root for its folder,
    /// as in a catalog, so it stays the same photograph there. None inside
    /// another session or catalog: that library's records would not see it.
    fn moved_place_for(&self, path: &Path) -> Result<Option<(String, PathBuf, String)>> {
        if let Some(place) = self.place_for(path)? {
            return Ok(Some(place));
        }
        if session_for(path).is_some() {
            return Ok(None);
        }
        let parent = path.parent().context("No parent")?;
        Ok(Some((
            external_root_key(parent),
            PathBuf::from(path.file_name().context("No filename")?),
            parent.to_string_lossy().into_owned(),
        )))
    }

    /// Whether the asset `id` may be recorded at `path` when it is found
    /// there (relink, folder sync): inside the library folder, anywhere for
    /// a catalog, and for a session photograph that already lives outside
    /// the session (moved there by the app) anywhere outside other libraries.
    fn placeable(&self, id: &str, path: &Path) -> Result<bool> {
        if self.place_for(path)?.is_some() {
            return Ok(true);
        }
        let root: Option<String> = self
            .conn
            .query_row("SELECT root_id FROM assets WHERE id=?", [id], |r| r.get(0))
            .optional()?;
        Ok(root.is_some_and(|root| root != "session") && session_for(path).is_none())
    }

    /// Records each asset at its new path (id, edits, metadata and albums
    /// kept), outside a session's folder too (`moved_place_for`); callers
    /// check first where a found file may be recorded (`placeable`). An
    /// asset moved into another library is kept as missing. External roots
    /// nothing is in any more are dropped.
    fn apply_places(&self, tx: &Connection, moved: &[(String, PathBuf)]) -> Result<()> {
        for (id, path) in moved {
            let Some((root, relative, root_path)) = self.moved_place_for(path)? else {
                tx.execute("UPDATE assets SET missing=1 WHERE id=?", [id])?;
                continue;
            };
            validate_relative(&relative)?;
            let rel = relative.to_str().context("Paths must be valid UTF-8")?;
            insert_root(tx, &root, &root_path)?;
            // A stale row for a file that is gone may hold the new place. It
            // keeps its edits under a name no file can have.
            tx.execute(
                "UPDATE assets SET rel_path=rel_path || '#missing-' || id, missing=1
                 WHERE root_id=? AND rel_path=? AND id!=?",
                params![root, rel, id],
            )?;
            tx.execute(
                "UPDATE assets SET root_id=?, rel_path=?, missing=0 WHERE id=?",
                params![root, rel, id],
            )?;
            forget_xmp_left_behind(tx, id, path)?;
        }
        tx.execute(
            "DELETE FROM roots WHERE external=1 AND id NOT IN (SELECT DISTINCT root_id FROM assets)",
            [],
        )?;
        Ok(())
    }

    /// Follows a rename or move done on disk (a file or a whole folder, from
    /// `from` to `to`): registered assets at or below `from` keep their id,
    /// edits and metadata at the new place, also when a session asset left
    /// the session folder (it is recorded there like a catalog's; one moved
    /// into another library is kept as missing). Returns how many assets
    /// moved.
    pub fn relocate(&self, from: &Path, to: &Path) -> Result<usize> {
        let (from, to) = (resolve_moved(from), resolve_moved(to));
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external FROM assets a JOIN roots r ON r.id=a.root_id",
        )?;
        let moved: Vec<(String, PathBuf)> = query
            .query_map([], |row| {
                let path = self.join_root(row.get(2)?, row.get(3)?, row.get(1)?);
                Ok((row.get::<_, String>(0)?, path))
            })?
            .filter_map(|row| {
                let (id, path) = row.ok()?;
                let below = path.strip_prefix(&from).ok()?;
                Some((
                    id,
                    if below.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(below)
                    },
                ))
            })
            .collect();
        drop(query);
        if moved.is_empty() {
            // Files coming back to where a missing asset was registered (a
            // move out of the session undone) are found again.
            return Ok(if to.is_file() {
                self.set_missing_at(&to, false)?
            } else {
                0
            });
        }
        let tx = self.conn.unchecked_transaction()?;
        self.apply_places(&tx, &moved)?;
        tx.commit()?;
        Ok(moved.len())
    }

    /// Every registered photograph whose file is missing (as of the last
    /// `verify_paths` or file operation), in path order, with the drive it
    /// is on when that drive is disconnected.
    pub fn offline_assets(&self) -> Result<Vec<OfflineAsset>> {
        self.offline_rows("a.missing=1", &[])
    }

    /// A registered photograph whose file is gone and that has the content
    /// (fingerprint) and file name of `path`: the same photograph renamed
    /// or moved into its folder outside Rawtonic (a folder renamed in
    /// Finder). Registering `path` would make a duplicate without its
    /// edits; `relink` follows it instead.
    pub fn vanished_as(&self, path: &Path) -> Result<Option<String>> {
        let name = path.file_name().context("No filename")?;
        let candidates: Vec<String> = self
            .conn
            .prepare("SELECT id FROM assets WHERE fingerprint=? AND size=?")?
            .query_map(
                params![
                    fingerprint(path)?,
                    i64::try_from(fs::metadata(path)?.len())?
                ],
                |r| r.get(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        for id in candidates {
            let asset = self.asset(&id)?;
            if asset.path.file_name() == Some(name) && !asset.path.is_file() {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Size and content fingerprint (`fingerprint`) of every registered
    /// photograph and recorded companion (a RAW's JPEG), in one query: a
    /// cheap test whether a file is already in the library before importing
    /// it (the import itself still compares SHA-256 before it skips a
    /// duplicate).
    pub fn content_keys(&self) -> Result<HashSet<(u64, String)>> {
        Ok(self
            .conn
            .prepare(
                "SELECT size, fingerprint FROM assets
                 UNION SELECT companion_size, companion_fingerprint FROM assets
                 WHERE companion_size IS NOT NULL AND companion_fingerprint IS NOT NULL",
            )?
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The asset as an offline asset (whether or not its file is there now).
    pub fn offline_asset(&self, id: &str) -> Result<OfflineAsset> {
        self.offline_rows("a.id=?", &[&id])?
            .pop()
            .context("Unknown asset")
    }

    /// Assets matching `filter` (SQL over `a` = assets, `r` = roots), as
    /// offline assets in path order.
    fn offline_rows(
        &self,
        filter: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<OfflineAsset>> {
        let mut query = self.conn.prepare(&format!(
            "SELECT a.id,a.rel_path,r.path,r.external,a.size,r.volume_name,r.volume_path,a.mtime,
                    r.volume_id
             FROM assets a JOIN roots r ON r.id=a.root_id WHERE {filter}"
        ))?;
        let mut connected: HashMap<(String, Option<String>), bool> = HashMap::new();
        let mut assets: Vec<OfflineAsset> = query
            .query_map(args, |row| {
                let path = self.join_root(row.get(2)?, row.get(3)?, row.get(1)?);
                let name: Option<String> = row.get(5)?;
                let mount: Option<String> = row.get(6)?;
                let id: Option<String> = row.get(8)?;
                // The drive is disconnected when its mount point is gone or
                // holds another volume now (a mount point folder that stays
                // when the drive is unmounted, as /mnt folders on Linux).
                let volume = name.filter(|_| {
                    mount.is_some_and(|m| {
                        !*connected
                            .entry((m.clone(), id.clone()))
                            .or_insert_with(|| volume_mounted_at(Path::new(&m), id.as_deref()))
                    })
                });
                Ok(OfflineAsset {
                    id: row.get(0)?,
                    path,
                    size: row.get::<_, i64>(4)? as u64,
                    mtime: row.get(7)?,
                    volume,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        assets.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(assets)
    }

    /// The photographs registered in `folder` (with `recursive`, also below
    /// it) whose file is not there now, so a folder listing keeps showing
    /// them. Reads only (the missing flags follow at the next check).
    pub fn offline_in(&self, folder: &Path, recursive: bool) -> Result<Vec<OfflineAsset>> {
        let folder = resolve_moved(folder);
        let like = |prefix: &str| {
            format!(
                "{}%",
                prefix
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        };
        let candidates = if let Ok(relative) = folder.strip_prefix(&self.directory) {
            let rel = relative.to_string_lossy();
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            self.offline_rows(
                "r.external=0 AND a.rel_path LIKE ? ESCAPE '\\'",
                &[&like(&prefix)],
            )?
        } else {
            let path = folder.to_string_lossy().into_owned();
            self.offline_rows(
                "r.external=1 AND (r.path=? OR r.path LIKE ? ESCAPE '\\')",
                &[&path, &like(&format!("{path}/"))],
            )?
        };
        Ok(candidates
            .into_iter()
            .filter(|asset| {
                (if recursive {
                    asset.path.starts_with(&folder)
                } else {
                    asset.path.parent() == Some(folder.as_path())
                }) && !asset.path.is_file()
            })
            .collect())
    }

    /// Relinks one photograph to the file the user located at `to`: the file
    /// must be the same photograph (size and the content fingerprint taken
    /// when it was registered), whatever its name. With `nearby`, the other
    /// missing photographs of its old folder (and below) are looked for at
    /// the same places in the new folder, as `relink_folder` does. Files are
    /// only read; the library's record of their place changes in one
    /// transaction.
    pub fn relink(&self, id: &str, to: &Path, nearby: bool) -> Result<RelinkReport> {
        let asset = self.asset(id)?;
        ensure!(
            !asset.path.is_file(),
            "This photograph is not missing: its file is at {}",
            asset.path.display()
        );
        let to = to
            .canonicalize()
            .context("The located file cannot be read")?;
        ensure!(to.is_file(), "Locate a file, not a folder");
        ensure!(
            fingerprint(&to)? == asset.fingerprint,
            "{} is not this photograph: its size or content differs",
            to.display()
        );
        ensure!(
            self.placeable(id, &to)?,
            "Session photographs must be inside the session folder; copy the file in with Ingest"
        );
        if let Some(other) = self.asset_at(&to)? {
            ensure!(
                other.id == asset.id,
                "{} is already another photograph of this library",
                to.display()
            );
        }
        let mut moved = vec![(asset.id.clone(), to.clone())];
        let mut report = RelinkReport::default();
        if nearby && let (Some(from), Some(into)) = (asset.path.parent(), to.parent()) {
            let (found, places) = self.relink_plan(from, into, Some(&asset.id))?;
            report = found;
            moved.extend(places);
        }
        let tx = self.conn.unchecked_transaction()?;
        self.apply_places(&tx, &moved)?;
        tx.commit()?;
        report.relinked.insert(0, (asset.path, to));
        Ok(report)
    }

    /// Relinks a folder that moved or was renamed (a drive copied or
    /// mounted elsewhere): every missing photograph registered in `from` or
    /// below is looked for at the same place in `to`, and relinked when that
    /// file is the same photograph (size and fingerprint). Files are only
    /// read; all relinks commit together.
    pub fn relink_folder(&self, from: &Path, to: &Path) -> Result<RelinkReport> {
        let to = to
            .canonicalize()
            .context("The located folder cannot be read")?;
        ensure!(to.is_dir(), "Locate a folder");
        let from = resolve_moved(from);
        // A session's photographs that lived outside it (moved there by the
        // app) may be found outside again; `relink_plan` checks each.
        ensure!(
            self.place_for(&to.join("_"))?.is_some() || !from.starts_with(&self.directory),
            "Session photographs must be inside the session folder; copy them in with Ingest"
        );
        let (report, moved) = self.relink_plan(&from, &to, None)?;
        let tx = self.conn.unchecked_transaction()?;
        self.apply_places(&tx, &moved)?;
        tx.commit()?;
        Ok(report)
    }

    /// Which missing photographs below `from` are at the same place below
    /// `to` (except `skip`), checked by size and fingerprint.
    fn relink_plan(
        &self,
        from: &Path,
        to: &Path,
        skip: Option<&str>,
    ) -> Result<(RelinkReport, Vec<(String, PathBuf)>)> {
        let mut report = RelinkReport::default();
        let mut moved = Vec::new();
        // Every registered photograph below `from` whose file is gone, not
        // only those already flagged missing: a folder moved in Finder since
        // the last check is relinked too.
        for asset in self.offline_rows("1=1", &[])? {
            let Ok(below) = asset.path.strip_prefix(from) else {
                continue;
            };
            if Some(asset.id.as_str()) == skip || asset.path.is_file() {
                continue;
            }
            let candidate = to.join(below);
            if !candidate.is_file() {
                report.not_found += 1;
                continue;
            }
            let registered = self.asset(&asset.id)?;
            let same = fs::metadata(&candidate).is_ok_and(|m| m.len() == registered.size)
                && fingerprint(&candidate).is_ok_and(|f| f == registered.fingerprint);
            if !same {
                report.mismatched.push(candidate);
                continue;
            }
            if self
                .asset_at(&candidate)?
                .is_some_and(|other| other.id != asset.id)
                || !self.placeable(&asset.id, &candidate)?
            {
                report.taken.push(candidate);
                continue;
            }
            report
                .relinked
                .push((asset.path.clone(), candidate.clone()));
            moved.push((asset.id, candidate));
        }
        Ok((report, moved))
    }

    /// Marks the asset registered at `path`, or every asset below it when it
    /// was a folder (sent to the system Trash, or put back from it), as
    /// missing or present. Returns how many matched.
    pub fn set_missing_at(&self, path: &Path, missing: bool) -> Result<usize> {
        let path = resolve_moved(path);
        let like = |prefix: &str| {
            format!(
                "{}/%",
                prefix
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        };
        let changed = if let Ok(relative) = path.strip_prefix(&self.directory) {
            let rel = relative.to_string_lossy();
            self.conn.execute(
                "UPDATE assets SET missing=? WHERE root_id='session'
                   AND (rel_path=? OR rel_path LIKE ? ESCAPE '\\')",
                params![missing, rel.as_ref(), like(&rel)],
            )?
        } else {
            let folder = path.to_string_lossy();
            let parent = path.parent().context("No parent")?.to_string_lossy();
            let name = path.file_name().context("No filename")?.to_string_lossy();
            self.conn.execute(
                "UPDATE assets SET missing=? WHERE
                   (rel_path=? AND root_id IN (SELECT id FROM roots WHERE external=1 AND path=?))
                   OR root_id IN (SELECT id FROM roots WHERE external=1
                                  AND (path=? OR path LIKE ? ESCAPE '\\'))",
                params![
                    missing,
                    name.as_ref(),
                    parent.as_ref(),
                    folder.as_ref(),
                    like(&folder)
                ],
            )?
        };
        Ok(changed)
    }

    /// Forgets for good the photographs registered at `path`, or at or
    /// below it when it was a folder, inside this library's own folder
    /// (files deleted permanently: the emptied session Trash). Their
    /// records go with their versions, recipes, masks, metadata and album
    /// memberships. Paths outside the library's folder are refused. Running
    /// it again finds nothing. Returns how many were forgotten.
    pub fn forget_at(&self, path: &Path) -> Result<usize> {
        let path = resolve_moved(path);
        let relative = path.strip_prefix(&self.directory).with_context(|| {
            format!(
                "{} is outside {}; its records are kept",
                path.display(),
                self.directory.display()
            )
        })?;
        ensure!(
            !relative.as_os_str().is_empty(),
            "The library folder itself cannot be forgotten"
        );
        validate_relative(relative)?;
        let rel = relative.to_str().context("Paths must be valid UTF-8")?;
        let like = format!(
            "{}/%",
            rel.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let ids: Vec<String> = tx
            .prepare(
                "SELECT id FROM assets WHERE root_id='session'
                   AND (rel_path=? OR rel_path LIKE ? ESCAPE '\\')",
            )?
            .query_map(params![rel, like], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            tx.execute(
                "DELETE FROM masks WHERE version_id IN (SELECT id FROM versions WHERE asset_id=?)",
                [id],
            )?;
            tx.execute("DELETE FROM assets WHERE id=?", [id])?;
        }
        prune_blobs(&tx)?;
        tx.commit()?;
        Ok(ids.len())
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }

    /// Every catalog folder (an external folder holding registered
    /// photographs, wherever they were found, followed or relinked to), with
    /// how many photographs it holds, how many of those are missing (as of
    /// the last check) and the drive it was registered on. In path order;
    /// nothing is read from disk.
    pub fn folders(&self) -> Result<Vec<FolderRecord>> {
        let mut query = self.conn.prepare(
            "SELECT r.path,r.volume_id,r.volume_name,r.volume_path,
                    count(a.id),coalesce(sum(a.missing),0),r.volume_kind
             FROM roots r JOIN assets a ON a.root_id=r.id
             WHERE r.external=1 GROUP BY r.id ORDER BY r.path",
        )?;
        let rows = query.query_map([], |r| {
            Ok(FolderRecord {
                path: PathBuf::from(r.get::<_, String>(0)?),
                volume_id: r.get(1)?,
                volume_name: r.get(2)?,
                volume_kind: r
                    .get::<_, Option<String>>(6)?
                    .map(|value| volume::VolumeKind::from_record(&value)),
                volume_path: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                photos: r.get::<_, i64>(4)? as usize,
                missing: r.get::<_, i64>(5)? as usize,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records the editor state of the virtual copy `copy` of `asset` (a
    /// version beside the primary one), so the copy stays listed and
    /// editable while the photograph is offline. The copy's own sidecar
    /// (`<photo>.<copy>.rrdata`) is derived from it while the file is there.
    /// Its snapshots are the library's (`Library::add_snapshot`); those the
    /// state carries are taken over only when the copy is first recorded
    /// (a copy's sidecar adopted).
    pub fn save_copy_state(&self, asset: &str, copy: &str, state: &Value) -> Result<()> {
        ensure!(valid_copy_id(copy), "Not a virtual copy id: {copy}");
        let carried = snapshots::in_state(state);
        let mut state = state.clone();
        snapshots::strip(&mut state);
        rawtonic_metadata::resources::localize(&mut state, &self.directory);
        let text = serde_json::to_string(&state)?;
        ensure!(text.len() <= 64 * 1024 * 1024, "Recipe too large");
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE versions SET legacy_state=? WHERE asset_id=? AND name=? AND is_primary=0",
            params![text, asset, copy],
        )?;
        if updated == 0 {
            let version = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO versions(id,asset_id,name,is_primary,legacy_state) VALUES(?,?,?,0,?)",
                params![version, asset, copy, text],
            )?;
            snapshots::insert(&tx, &self.directory, &version, &carried)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The recorded editor state of a virtual copy, with its snapshots.
    pub fn copy_state(&self, asset: &str, copy: &str) -> Result<Option<Value>> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id,legacy_state FROM versions WHERE asset_id=? AND name=? AND is_primary=0",
                params![asset, copy],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((version, Some(text))) = row else {
            return Ok(None);
        };
        let mut value = serde_json::from_str(&text)?;
        rawtonic_metadata::resources::resolve(&mut value, &self.directory);
        let snapshots = self.snapshots(&version)?;
        if !snapshots.is_empty() {
            snapshots::set_in_state(&mut value, &snapshots)?;
        }
        Ok(Some(value))
    }

    /// The ids of the virtual copies recorded for `asset`, in id order.
    pub fn copies(&self, asset: &str) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare("SELECT name FROM versions WHERE asset_id=? AND is_primary=0 ORDER BY name")?
            .query_map([asset], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets a virtual copy (its sidecar was deleted). Returns whether one was recorded.
    pub fn remove_copy(&self, asset: &str, copy: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM versions WHERE asset_id=? AND name=? AND is_primary=0",
            params![asset, copy],
        )? > 0;
        prune_blobs(&self.conn)?;
        Ok(removed)
    }
}

/// A virtual copy id as the editor makes them (six lower-case hex digits,
/// the `<photo>.<id>.rrdata` sidecar name).
pub fn valid_copy_id(copy: &str) -> bool {
    copy.len() == 6 && copy.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A catalog folder as `Library::folders` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRecord {
    pub path: PathBuf,
    /// Registered photographs in it.
    pub photos: usize,
    /// Of those, the ones whose file was missing at the last check.
    pub missing: usize,
    /// The drive it was registered on (identity, name, mount point then).
    pub volume_id: Option<String>,
    pub volume_name: Option<String>,
    pub volume_kind: Option<volume::VolumeKind>,
    pub volume_path: Option<PathBuf>,
}

/// After an asset moved to `path` (relinked, or a move followed): the XMP
/// digest the library keeps belongs to the sidecar at the old place. When the
/// sidecar did not come along (only the photograph was moved), the next sync
/// would take the missing file for an external change and refuse forever, so
/// the digest is dropped: with no XMP at the new place the next sync writes
/// one, and an unrelated XMP there stays a conflict to review, never
/// replaced. A sidecar that moved with the photograph keeps its digest.
fn forget_xmp_left_behind(tx: &Connection, id: &str, path: &Path) -> Result<()> {
    let hash: Option<String> = tx
        .query_row(
            "SELECT xmp_hash FROM metadata WHERE asset_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(hash) = hash else { return Ok(()) };
    let here = fs::read(path.with_extension("xmp"))
        .ok()
        .map(|bytes| rawtonic_metadata::digest(&bytes));
    if here.as_deref() != Some(hash.as_str()) {
        tx.execute(
            "UPDATE metadata SET xmp_hash=NULL, xmp_status='pending' WHERE asset_id=?",
            [id],
        )?;
    }
    Ok(())
}

/// Makes `payload` the head recipe of `version` (a new history entry unless
/// it is the head already).
fn write_recipe(tx: &Connection, version: &str, payload: &str) -> Result<()> {
    let mut stored: Value = serde_json::from_str(payload)?;
    let hashes = store_blobs(tx, &mut stored)?;
    let payload = serde_json::to_string(&stored)?;
    let payload = payload.as_str();
    let head: Option<i64> =
        tx.query_row("SELECT head FROM versions WHERE id=?", [version], |r| {
            r.get(0)
        })?;
    let old: Option<String> = head
        .map(|id| tx.query_row("SELECT payload FROM recipes WHERE id=?", [id], |r| r.get(0)))
        .transpose()?;
    if old.as_deref() != Some(payload) {
        tx.execute(
            "INSERT INTO recipes(version_id,parent,payload) VALUES(?,?,?)",
            params![version, head, payload],
        )?;
        let id = tx.last_insert_rowid();
        refer_blobs(tx, id, &hashes)?;
        tx.execute(
            "UPDATE versions SET head=? WHERE id=?",
            params![id, version],
        )?;
    }
    Ok(())
}

/// Recipe strings from this length on are stored once (`recipe_blobs`).
pub(crate) const BLOB_MIN_LEN: usize = 16 * 1024;
const BLOB_PREFIX: &str = "rawtonic-blob:sha256:";

/// Moves the long strings of `value` (data URLs of masks and analyses) into
/// `recipe_blobs`, leaving references; returns their hashes.
fn store_blobs(tx: &Connection, value: &mut Value) -> Result<Vec<String>> {
    let mut hashes = Vec::new();
    let mut error = None;
    visit_strings(value, &mut |text| {
        if text.len() < BLOB_MIN_LEN || error.is_some() {
            return;
        }
        let hash: String = <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        match tx.execute(
            "INSERT OR IGNORE INTO recipe_blobs(hash,data) VALUES(?,?)",
            params![hash, text.as_str()],
        ) {
            Ok(_) => {
                *text = format!("{BLOB_PREFIX}{hash}");
                hashes.push(hash);
            }
            Err(e) => error = Some(e),
        }
    });
    if let Some(e) = error {
        return Err(e.into());
    }
    hashes.sort();
    hashes.dedup();
    Ok(hashes)
}

fn refer_blobs(tx: &Connection, recipe: i64, hashes: &[String]) -> Result<()> {
    for hash in hashes {
        tx.execute(
            "INSERT OR IGNORE INTO recipe_blob_refs(recipe_id,hash) VALUES(?,?)",
            params![recipe, hash],
        )?;
    }
    Ok(())
}

/// Puts the stored strings back in place of their references.
fn load_blobs(conn: &Connection, value: &mut Value) -> Result<()> {
    let mut error = None;
    visit_strings(value, &mut |text| {
        let Some(hash) = text.strip_prefix(BLOB_PREFIX) else {
            return;
        };
        if error.is_some() {
            return;
        }
        match conn.query_row("SELECT data FROM recipe_blobs WHERE hash=?", [hash], |r| {
            r.get::<_, String>(0)
        }) {
            Ok(data) => *text = data,
            Err(e) => error = Some(e),
        }
    });
    match error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

fn visit_strings(value: &mut Value, visit: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => visit(text),
        Value::Array(items) => items.iter_mut().for_each(|v| visit_strings(v, visit)),
        Value::Object(map) => map.values_mut().for_each(|v| visit_strings(v, visit)),
        _ => {}
    }
}

/// Drops stored strings no recipe refers to any more (after removals).
pub(crate) fn prune_blobs(tx: &Connection) -> Result<()> {
    tx.execute(
        "DELETE FROM recipe_blobs WHERE hash NOT IN (SELECT hash FROM recipe_blob_refs)
         AND hash NOT IN (SELECT hash FROM snapshot_blob_refs)",
        [],
    )?;
    Ok(())
}

/// Stores an asset's metadata (its XMP is to be written again) and the
/// search text it gives.
fn write_metadata(tx: &Connection, id: &str, metadata: &Metadata) -> Result<()> {
    ensure!(
        tx.execute(
            "UPDATE metadata SET rating=?,label=?,flag=?,data=?,xmp_status='pending',from_camera=0 WHERE asset_id=?",
            params![
                metadata.rating,
                metadata.label,
                metadata.flag,
                serde_json::to_string(metadata)?,
                id
            ]
        )? == 1,
        "Unknown asset"
    );
    tx.execute(
        "UPDATE assets SET search_text=? WHERE id=?",
        params![search_text(metadata), id],
    )?;
    Ok(())
}

/// What a library search finds an asset by besides its path: keywords,
/// title, description, creator and the IPTC fields (city, country, …).
fn search_text(metadata: &Metadata) -> String {
    format!(
        "{} {} {} {} {}",
        metadata.keywords.join(" "),
        metadata.title,
        metadata.description,
        metadata.creator,
        metadata
            .iptc
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// The editor's prefix for keywords the user typed (other tags come from
/// automatic tagging).
const USER_TAG_PREFIX: &str = "user:";

/// A photograph's editor state with the library's rating, keywords and
/// label. A photograph never edited has no state of its own; when the library
/// holds such metadata for it (set by `rawtonic rate`, adopted from an XMP or
/// imported), it gets a state that carries it, so the app shows what the
/// library holds. Its adjustments stay null: the photo still opens with the
/// defaults of a never-edited photo, not as an (empty) saved recipe.
fn with_library_metadata(state: Option<Value>, meta: Metadata) -> Option<Value> {
    let mut state = match state {
        Some(state) => state,
        None if meta.rating > 0 || !meta.keywords.is_empty() || !meta.label.is_empty() => {
            json!({"version": 1, "rating": 0, "tags": [], "adjustments": null})
        }
        None => return None,
    };
    apply_editor_metadata(&mut state, meta);
    Some(state)
}

fn apply_editor_metadata(state: &mut Value, meta: Metadata) {
    state["rating"] = json!(meta.rating);
    // Keywords the stored state has as automatic tags stay automatic; all
    // others, including keywords adopted from an XMP, are the user's.
    let automatic: HashSet<String> = state
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|tag| !tag.starts_with(USER_TAG_PREFIX) && !tag.starts_with("color:"))
        .map(str::to_string)
        .collect();
    let mut tags: Vec<String> = meta
        .keywords
        .into_iter()
        .map(|keyword| {
            if automatic.contains(&keyword) || keyword.starts_with(USER_TAG_PREFIX) {
                keyword
            } else {
                format!("{USER_TAG_PREFIX}{keyword}")
            }
        })
        .collect();
    if !meta.label.is_empty() {
        tags.push(format!("color:{}", meta.label.to_lowercase()));
    }
    state["tags"] = json!(tags);
    if let Some(a) = state.get_mut("adjustments").filter(|a| a.is_object()) {
        a["colorLabel"] = json!(meta.label);
    }
}

/// Whether the volume `id` recorded at `mount` is mounted there now: the
/// folder exists and, when both identities are known, holds that volume (not
/// an empty mount point folder on the system disk). Without an identity the
/// folder decides.
fn volume_mounted_at(mount: &Path, id: Option<&str>) -> bool {
    if !mount.is_dir() {
        return false;
    }
    match (id, volume::volume_of(mount)) {
        (Some(id), Some(now)) => now.id == id,
        _ => true,
    }
}

/// Registers a root once. A new external root records the volume its
/// folder is on (identity, name, mount point) for `follow_volumes` and for
/// telling a disconnected drive from a missing file.
fn insert_root(conn: &Connection, root: &str, root_path: &str) -> Result<()> {
    let external = root != "session";
    let added = conn.execute(
        "INSERT OR IGNORE INTO roots(id,path,external) VALUES(?,?,?)",
        params![root, root_path, i32::from(external)],
    )?;
    if added == 1
        && external
        && let Some(volume) = volume::volume_of(Path::new(root_path))
    {
        record_volume(conn, root, &volume)?;
    }
    Ok(())
}

fn record_volume(conn: &Connection, root: &str, volume: &Volume) -> Result<()> {
    conn.execute(
        "UPDATE roots SET volume_id=?,volume_name=?,volume_path=?,volume_kind=? WHERE id=?",
        params![
            volume.id,
            volume.name,
            volume.mount_point.to_string_lossy().as_ref(),
            volume.kind.as_str(),
            root
        ],
    )?;
    Ok(())
}

impl Library {
    /// Records `volume` as the drive of the catalog folder `folder` (normally
    /// read from the system when the folder is registered; tests and tools
    /// set it directly).
    pub fn set_folder_volume(&self, folder: &Path, volume: &Volume) -> Result<()> {
        let folder = resolve_moved(folder);
        ensure!(
            self.conn.query_row(
                "SELECT count(*) FROM roots WHERE external=1 AND path=?",
                [folder.to_string_lossy().as_ref()],
                |r| r.get::<_, i64>(0)
            )? == 1,
            "No catalog folder is registered at {}",
            folder.display()
        );
        record_volume(&self.conn, &external_root_key(&folder), volume)
    }
}

/// The root id of an external (catalog) folder.
fn external_root_key(folder: &Path) -> String {
    format!(
        "external-{:032x}",
        xxhash_rust::xxh3::xxh3_128(folder.to_string_lossy().as_bytes())
    )
}

/// The canonical form of a path that may no longer (or not yet) exist: its
/// nearest existing ancestor canonicalized, the rest appended unchanged.
fn resolve_moved(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    for ancestor in path.ancestors().skip(1) {
        if let Ok(canonical) = ancestor.canonicalize() {
            return canonical.join(path.strip_prefix(ancestor).unwrap_or(path));
        }
    }
    path.to_path_buf()
}

/// Resolve only explicit Rawtonic session manifests; ordinary folders stay standalone.
pub fn session_for(path: &Path) -> Option<PathBuf> {
    path.parent()?
        .ancestors()
        .find(|p| p.join("session.json").is_file() || p.join("catalog.json").is_file())
        .map(Path::to_path_buf)
}

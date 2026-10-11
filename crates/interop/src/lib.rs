//! Read-only readers for other editors' libraries (G50): Lightroom Classic
//! catalogs (`.lrcat`) and Capture One sessions (`.cosessiondb`).
//!
//! Both are SQLite files the other application owns. They are never opened
//! for writing: each reader copies the database (and a write-ahead log
//! beside it) into a temporary folder and reads the copy, so the original
//! stays byte-identical and a running editor is not disturbed. The readers
//! return the photographs with their metadata, the editor's own develop
//! settings (translated into Rawtonic's tools by the app, which owns those
//! converters) and the albums.
use anyhow::{Context, Result, ensure};
use rawtonic_metadata::Metadata;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub mod capture_one;
pub mod lightroom;

/// The develop settings of a photograph, in its editor's own form.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum Develop {
    None,
    /// The XMP packet Lightroom keeps in its catalog (`crs:` settings).
    LightroomXmp(String),
    /// Capture One settings as style entries (`<E K="Exposure" V="0.5"/>`).
    CaptureOne(Vec<(String, String)>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    /// The editor's id for the photo (unique within the source).
    pub key: String,
    pub path: PathBuf,
    pub metadata: Metadata,
    pub develop: Develop,
    /// For a virtual copy (Lightroom) or a further variant (Capture One):
    /// the key of the photograph it belongs to.
    pub copy_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub key: String,
    pub name: String,
    pub parent: Option<String>,
    /// A collection set / album group, holding albums rather than photos.
    pub is_group: bool,
    pub photos: Vec<String>,
}

/// What a reader found but does not bring over.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotRead {
    pub smart_albums: usize,
    pub local_adjustments: usize,
    /// Photos whose folder the library no longer records.
    pub unlocated: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub name: String,
    pub photos: Vec<Photo>,
    pub albums: Vec<Album>,
    pub not_read: NotRead,
}

impl Library {
    /// Points photos under `from` at `to` instead (a moved drive or folder).
    pub fn relink(&mut self, from: &Path, to: &Path) -> usize {
        let mut changed = 0;
        for photo in &mut self.photos {
            if let Ok(rest) = photo.path.strip_prefix(from) {
                photo.path = to.join(rest);
                changed += 1;
            }
        }
        changed
    }

    /// The top folders the photos live in, with how many photos each holds
    /// and how many of those exist, for relinking.
    pub fn roots(&self) -> Vec<Root> {
        let mut roots: Vec<Root> = Vec::new();
        for photo in self.photos.iter().filter(|p| p.copy_of.is_none()) {
            let folder = photo.path.parent().unwrap_or(Path::new("/")).to_path_buf();
            let exists = photo.path.is_file();
            match roots.iter_mut().find(|r| r.folder == folder) {
                Some(root) => {
                    root.photos += 1;
                    root.found += usize::from(exists);
                }
                None => roots.push(Root {
                    folder,
                    photos: 1,
                    found: usize::from(exists),
                }),
            }
        }
        roots.sort_by(|a, b| a.folder.cmp(&b.folder));
        roots
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub folder: PathBuf,
    pub photos: usize,
    pub found: usize,
}

/// Rawtonic's colour labels (Lightroom's default set).
pub const LABELS: [&str; 5] = ["Red", "Yellow", "Green", "Blue", "Purple"];

/// A label name in one of the common interface languages, or `""`.
pub fn label_from_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let label = match lower.as_str() {
        "red" | "rot" | "rouge" | "rojo" | "rosso" | "vermelho" | "rood" | "czerwony"
        | "красный" | "赤" | "红色" | "紅色" | "빨간색" => "Red",
        "yellow" | "gelb" | "jaune" | "amarillo" | "giallo" | "amarelo" | "geel" | "żółty"
        | "желтый" | "黄" | "黄色" | "노란색" => "Yellow",
        "green" | "grün" | "vert" | "verde" | "groen" | "zielony" | "зеленый" | "緑" | "绿色"
        | "綠色" | "녹색" => "Green",
        "blue" | "blau" | "bleu" | "azul" | "blu" | "blauw" | "niebieski" | "синий" | "青"
        | "蓝色" | "藍色" | "파란색" => "Blue",
        "purple"
        | "violett"
        | "lila"
        | "violet"
        | "morado"
        | "púrpura"
        | "viola"
        | "roxo"
        | "paars"
        | "fioletowy"
        | "фиолетовый"
        | "紫"
        | "紫色"
        | "보라색" => "Purple",
        _ => "",
    };
    label.to_string()
}

/// Opens a copy of another application's SQLite database, read-only. The
/// copy (with its write-ahead log, which holds the newest changes) lives in
/// `scratch` for as long as the returned connection is used.
pub(crate) fn open_copy(database: &Path, scratch: &Path) -> Result<Connection> {
    ensure!(database.is_file(), "{} was not found", database.display());
    let name = database.file_name().context("No file name")?;
    let copy = scratch.join(name);
    fs::copy(database, &copy).with_context(|| format!("Cannot read {}", database.display()))?;
    for suffix in ["-wal", "-shm"] {
        let mut side = database.as_os_str().to_owned();
        side.push(suffix);
        let side = PathBuf::from(side);
        if side.is_file() {
            let mut target = copy.as_os_str().to_owned();
            target.push(suffix);
            fs::copy(&side, PathBuf::from(target))?;
        }
    }
    // The copy may replay its log, which needs write access to the copy only.
    let conn = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA query_only=ON;")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_names_in_several_languages() {
        assert_eq!(label_from_name("Rot"), "Red");
        assert_eq!(label_from_name(" violet "), "Purple");
        assert_eq!(label_from_name("Orange"), "");
    }

    #[test]
    fn the_original_database_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.lrcat");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(1);")
            .unwrap();
        let before = fs::read(&db).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let conn = open_copy(&db, scratch.path()).unwrap();
        assert!(
            conn.execute("INSERT INTO t VALUES(2)", []).is_err(),
            "query only"
        );
        drop(conn);
        assert_eq!(fs::read(&db).unwrap(), before);
        assert!(!dir.path().join("x.lrcat-wal").exists());
    }
}

//! A session's Capture, Selects and Output folders (G38). Each can be any
//! folder inside the session (stored relative, so a closed session can be
//! moved or copied anywhere) or outside it (stored absolute: a capture
//! drive, an export share), as in Capture One. Changing a folder never
//! moves files; the app offers to move the old folder's contents with the
//! journaled file operations. A folder outside the session that is not
//! there (a drive not connected) is reported as unavailable and work into
//! it fails clearly: it is never created or replaced by another folder.
//!
//! Photographs in a session's folders outside it are registered like a
//! catalog's (an external root per folder), so they keep their edits while
//! the drive is away.
use crate::{Library, Manifest, Mode, manifest_name, portable_relative, validate_relative};
use anyhow::{Context, Result, bail, ensure};
use rawtonic_metadata::atomic_write;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// A folder of the session with a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    /// Where captures and imports go.
    Capture,
    /// Where "Move to Selects" moves photographs.
    Selects,
    /// The default destination of exports.
    Output,
}

impl FolderRole {
    pub const ALL: [FolderRole; 3] = [FolderRole::Capture, FolderRole::Selects, FolderRole::Output];

    /// The folder a new session has (relative to it).
    pub fn default_setting(self) -> &'static str {
        match self {
            FolderRole::Capture => "Capture",
            FolderRole::Selects => "Selects",
            FolderRole::Output => "Output",
        }
    }

    fn name(self) -> &'static str {
        self.default_setting()
    }
}

pub(crate) fn default_selects() -> String {
    FolderRole::Selects.default_setting().into()
}

/// Folders of the session that hold its own data or are left to the app:
/// never a Capture, Selects or Output folder.
const RESERVED: &[&str] = &[
    "Trash",
    "masks",
    "resources",
    "profiles",
    "previews",
    "smart-previews",
];

/// One folder as the session settings show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFolder {
    pub role: FolderRole,
    /// As stored in `session.json`: relative inside, absolute outside.
    pub stored: String,
    /// Where it is now.
    pub path: PathBuf,
    /// Inside the session folder (moves with it).
    pub inside: bool,
    /// The folder is there (false: deleted, or its drive is not connected).
    pub available: bool,
    /// Whether `path` is a folder the app may make again when it is
    /// missing: only a folder inside the session. One outside it (a drive
    /// that is away) is never made: work into it fails, naming it.
    pub recreated: bool,
}

/// Whether a stored folder setting is absolute on any platform (`/…`,
/// `C:\…`, `\\server\…`): a session made on Windows and opened on a Mac
/// shows that folder as not available rather than refusing to open.
pub fn is_absolute_setting(setting: &str) -> bool {
    let bytes = setting.as_bytes();
    Path::new(setting).is_absolute()
        || setting.starts_with('/')
        || setting.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

/// Checks the folder settings of a manifest read from disk: a catalog's are
/// relative; a session's relative or absolute.
pub(crate) fn validate_manifest(manifest: &Manifest) -> Result<()> {
    for setting in [&manifest.capture, &manifest.selects, &manifest.output] {
        if manifest.mode == Mode::Session && is_absolute_setting(setting) {
            continue;
        }
        validate_relative(Path::new(setting))?;
    }
    ensure!(
        manifest
            .former_folders
            .iter()
            .all(|f| is_absolute_setting(f)),
        "A former session folder must be an absolute path"
    );
    ensure!(
        manifest.output_folders.iter().all(|f| {
            manifest.mode == Mode::Session
                && (is_absolute_setting(f) || validate_relative(Path::new(f)).is_ok())
        }),
        "An Output folder must be a path inside the session, or an absolute one"
    );
    Ok(())
}

impl Manifest {
    pub fn folder_setting(&self, role: FolderRole) -> &str {
        match role {
            FolderRole::Capture => &self.capture,
            FolderRole::Selects => &self.selects,
            FolderRole::Output => &self.output,
        }
    }

    fn folder_setting_mut(&mut self, role: FolderRole) -> &mut String {
        match role {
            FolderRole::Capture => &mut self.capture,
            FolderRole::Selects => &mut self.selects,
            FolderRole::Output => &mut self.output,
        }
    }

    /// The session's folders outside the library folder (absolute): those
    /// set now and those set before (`former_folders`), whose photographs
    /// stay the session's.
    pub fn outside_folders(&self) -> Vec<PathBuf> {
        if self.mode != Mode::Session {
            return Vec::new();
        }
        let mut folders: Vec<PathBuf> = FolderRole::ALL
            .iter()
            .map(|role| self.folder_setting(*role))
            .chain(self.former_folders.iter().map(String::as_str))
            .filter(|setting| is_absolute_setting(setting))
            .map(PathBuf::from)
            .collect();
        folders.sort();
        folders.dedup();
        folders
    }
}

/// The folders outside the session at `directory` (read from its
/// `session.json`; none for a catalog or when it cannot be read).
pub fn outside_folders_of(directory: &Path) -> Vec<PathBuf> {
    fs::read(directory.join(manifest_name(Mode::Session)))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .filter(|manifest| validate_manifest(manifest).is_ok())
        .map(|manifest| manifest.outside_folders())
        .unwrap_or_default()
}

impl Library {
    /// Where the folder with `role` is (it may not be there).
    pub fn folder_path(&self, role: FolderRole) -> PathBuf {
        let setting = self.manifest.folder_setting(role);
        if is_absolute_setting(setting) {
            PathBuf::from(setting)
        } else {
            self.directory.join(setting)
        }
    }

    pub fn session_folders(&self) -> Vec<SessionFolder> {
        FolderRole::ALL
            .iter()
            .map(|role| self.session_folder(*role))
            .collect()
    }

    pub fn session_folder(&self, role: FolderRole) -> SessionFolder {
        let stored = self.manifest.folder_setting(role).to_string();
        let path = self.folder_path(role);
        SessionFolder {
            role,
            inside: !is_absolute_setting(&stored),
            available: path.is_dir(),
            recreated: stored == role.default_setting()
                || (role == FolderRole::Output && self.manifest.output_folders.is_empty()),
            stored,
            path,
        }
    }

    /// The folder with `role`, to put files in now. A folder the app made
    /// itself when the session was new (its default, a path inside the
    /// session) is made again when someone removed it. Any other folder
    /// that is not there fails, naming it, and nothing is made in its
    /// place: a configured folder inside the session whose parent was
    /// renamed or which was removed is not quietly recreated at the old
    /// path, where it would take the work meant for the renamed one.
    pub fn usable_folder(&self, role: FolderRole) -> Result<PathBuf> {
        let folder = self.session_folder(role);
        if folder.available {
            return Ok(folder.path);
        }
        if folder.recreated {
            fs::create_dir_all(&folder.path)?;
            return Ok(folder.path);
        }
        if folder.inside {
            bail!(
                "The {} folder {} is not available ({}) and was not made again. Nothing was written; choose another {} folder in Session Folders, or make the folder again and reopen the session.",
                role.name(),
                folder.path.display(),
                if folder.stored.is_empty()
                    || folder.path.parent().is_none_or(|parent| parent.is_dir())
                {
                    "is it called something else now?"
                } else {
                    "its parent folder was renamed or moved"
                },
                role.name()
            );
        }
        bail!(
            "The {} folder {} is not available (is its drive connected?). Nothing was written; connect the drive or choose another {} folder in Session Settings.",
            role.name(),
            folder.path.display(),
            role.name()
        )
    }

    /// Whether `path` is the session's Output folder or below it (exports,
    /// not photographs: scans leave it out wherever it is), or in one of
    /// the Output folders it had before (`output_folders`): a former Output
    /// folder under a scan root still holds the exports made there, so it
    /// is no photograph either.
    ///
    /// A session written before `output_folders` was kept (schema 23) has
    /// only the default `Output` of that time; until the first Output
    /// change records the folders, that one is left out as well, by its
    /// setting under the session folder. Nothing else is ever answered by a
    /// name.
    pub fn in_output_folder(&self, path: &Path) -> bool {
        if self.manifest.mode != Mode::Session {
            return false;
        }
        let current = self.folder_path(FolderRole::Output);
        // The folders as they are stored; beside them where the current one
        // really is, when the session folder is reached through a link (a
        // scan lists files under the path it walks, and leaves a folder only
        // when it is given that very path).
        let walked = current
            .is_dir()
            .then(|| current.canonicalize().ok())
            .flatten();
        path.starts_with(&current)
            || walked.as_ref().is_some_and(|real| path.starts_with(real))
            || self
                .former_output_folders()
                .iter()
                .any(|folder| path.starts_with(folder))
    }

    /// Where every folder the session had as Output is now, the current one
    /// left out.
    fn former_output_folders(&self) -> Vec<PathBuf> {
        let mut stored: Vec<&str> = self
            .manifest
            .output_folders
            .iter()
            .map(String::as_str)
            .collect();
        if stored.is_empty() {
            // A session from before `output_folders`: its Output setting at
            // that time was the default, written and not changed.
            stored.push(FolderRole::Output.default_setting());
        }
        stored
            .into_iter()
            .filter(|setting| *setting != self.manifest.output)
            .map(|setting| {
                if is_absolute_setting(setting) {
                    PathBuf::from(setting)
                } else {
                    self.directory.join(setting)
                }
            })
            .collect()
    }

    /// Whether `path` is in one of the session's folders outside the
    /// library folder (`path` canonical, or its folder canonical).
    pub fn in_outside_folder(&self, path: &Path) -> bool {
        self.manifest
            .outside_folders()
            .iter()
            .any(|folder| path.starts_with(folder))
    }

    /// Makes `folder` the session's folder with `role`. It must exist; it is
    /// stored relative when it is inside the session, absolute otherwise.
    /// Nothing is moved: the files in the old folder stay there (and stay
    /// registered). Refused: a catalog, the session folder itself or a
    /// folder holding it, a folder the session keeps for its own data, a
    /// folder of another library, and a folder another role already has.
    pub fn set_session_folder(&mut self, role: FolderRole, folder: &Path) -> Result<SessionFolder> {
        ensure!(
            self.manifest.mode == Mode::Session,
            "Only a session's folders can be chosen; a catalog keeps its photographs where they are"
        );
        let real = folder
            .canonicalize()
            .with_context(|| format!("{} is not available", folder.display()))?;
        ensure!(real.is_dir(), "{} is not a folder", folder.display());
        ensure!(
            !self.directory.starts_with(&real),
            "{} holds this session; choose a folder inside it or beside it",
            real.display()
        );
        let stored = if let Ok(relative) = real.strip_prefix(&self.directory) {
            let first = relative
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            ensure!(
                !RESERVED.contains(&first.as_str())
                    && !relative
                        .components()
                        .any(|c| c.as_os_str().to_string_lossy().starts_with('.')),
                "{} keeps the session's own data and cannot be its {} folder",
                real.display(),
                role.name()
            );
            portable_relative(relative)?
        } else {
            if let Some(other) = real.ancestors().find(|dir| {
                dir.join("session.json").is_file() || dir.join("catalog.json").is_file()
            }) {
                bail!(
                    "{} belongs to the library {}; choose a folder outside it",
                    real.display(),
                    other.display()
                );
            }
            real.to_str()
                .with_context(|| format!("{} has a name that cannot be stored", real.display()))?
                .to_string()
        };
        // A hot folder feeding the session would take its own captures again.
        if let Some(hot) = self.hot_folder()? {
            ensure!(
                !real.starts_with(&hot.folder) && !hot.folder.starts_with(&real),
                "{} is the session's hot folder or holds it; choose another folder",
                real.display()
            );
        }
        for other in FolderRole::ALL.into_iter().filter(|r| *r != role) {
            ensure!(
                self.folder_path(other) != real,
                "{} is already the session's {} folder",
                real.display(),
                other.name()
            );
        }
        let mut manifest = self.manifest.clone();
        let previous = std::mem::replace(manifest.folder_setting_mut(role), stored.clone());
        // Every folder that was the Output folder stays on record (also one
        // outside the session chosen again later): exports made there never
        // become photographs.
        if role == FolderRole::Output && !manifest.output_folders.contains(&previous) {
            manifest.output_folders.push(previous.clone());
        }
        // A folder outside left behind keeps its photographs the session's
        // (and an Undo of "Move contents" can put them back there).
        manifest.former_folders.retain(|f| *f != stored);
        // Only Capture and Selects hold photographs: a former Output folder
        // holds exports, which no scan may take for photographs.
        if role != FolderRole::Output
            && is_absolute_setting(&previous)
            && previous != stored
            && !manifest.former_folders.contains(&previous)
        {
            manifest.former_folders.push(previous);
        }
        self.write_manifest(manifest)?;
        Ok(self.session_folder(role))
    }

    /// Puts every folder outside the session back to its default inside it
    /// (a complete copy of the session holds its photographs itself).
    pub fn reset_outside_folders(&mut self) -> Result<()> {
        let mut manifest = self.manifest.clone();
        for role in FolderRole::ALL {
            if is_absolute_setting(manifest.folder_setting(role)) {
                *manifest.folder_setting_mut(role) = role.default_setting().into();
                fs::create_dir_all(self.directory.join(role.default_setting()))?;
            }
        }
        manifest.former_folders.clear();
        self.write_manifest(manifest)
    }

    fn write_manifest(&mut self, manifest: Manifest) -> Result<()> {
        validate_manifest(&manifest)?;
        atomic_write(
            &self.directory.join(manifest_name(manifest.mode)),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        self.manifest = manifest;
        Ok(())
    }
}

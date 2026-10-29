// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Shell uses ordinary paths; the library retains verbatim native paths.
//! Verify the entry the Shell parsed before allowing a recycle operation.
use super::PlatformTrash;
use anyhow::{Context, Result, ensure};
use rawtonic_fileops::SystemTrash;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Storage::EnhancedStorage::PKEY_FileName,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FileDispositionInfo, GetFileInformationByHandle,
            SetFileInformationByHandle,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize, StructuredStorage::PropVariantToBSTR,
        },
        UI::Shell::{
            BHID_EnumItems, FOF_NO_CONNECTED_ELEMENTS, FOF_NO_UI, FOFX_EARLYFAILURE,
            FOFX_RECYCLEONDELETE, FOLDERID_RecycleBinFolder, FileOperation, IEnumShellItems,
            IFileOperation, IShellItem, IShellItem2, KF_FLAG_DEFAULT, PID_DISPLACED_FROM,
            PSGUID_DISPLACED, PropertiesSystem::PROPERTYKEY, SHCreateItemFromParsingName,
            SHGetKnownFolderItem, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
        },
    },
    core::{HSTRING, Interface},
};

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        // A caller with an incompatible COM apartment fails before any mutation.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn plain_drive_path(path: &Path) -> Result<PathBuf> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        anyhow::bail!("The Recycle Bin requires an absolute local drive path");
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => anyhow::bail!("The Recycle Bin does not support this path namespace"),
    };
    ensure!(
        matches!(components.next(), Some(Component::RootDir)),
        "Drive-relative path"
    );
    let mut plain = PathBuf::from(format!("{}:\\", char::from(drive)));
    plain.push(components.as_path());
    Ok(plain)
}

fn identity(path: &Path) -> Result<(u32, u32, u32)> {
    // Do not follow a leaf symlink/junction when identifying the directory entry.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    file_identity(&file)
}

fn file_identity(file: &File) -> Result<(u32, u32, u32)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle() as isize), &mut info)? };
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

fn item_path(item: &IShellItem) -> Result<PathBuf> {
    unsafe {
        let text = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(OsString::from_wide(text.as_wide()));
        CoTaskMemFree(Some(text.0.cast()));
        Ok(path)
    }
}

struct BinEntry {
    id: String,
    original: PathBuf,
    item: IShellItem,
}

fn bin_entries() -> Result<Vec<BinEntry>> {
    // Keep the virtual Bin item while reading properties. Re-parsing its
    // desktop parsing name yields the physical $R file, whose filename is
    // different; display names can also omit known extensions.
    unsafe {
        let bin: IShellItem = SHGetKnownFolderItem(
            &FOLDERID_RecycleBinFolder,
            KF_FLAG_DEFAULT,
            HANDLE::default(),
        )?;
        let enumeration: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems)?;
        let mut entries = Vec::new();
        loop {
            let mut fetched = 0;
            let mut slot = [None];
            enumeration.Next(&mut slot, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            let item = slot[0].take().context("Missing enumerated Bin item")?;
            let properties: IShellItem2 = item.cast()?;
            let original_parent = PropVariantToBSTR(&properties.GetProperty(&PROPERTYKEY {
                fmtid: PSGUID_DISPLACED,
                pid: PID_DISPLACED_FROM,
            })?)?;
            let parent = PathBuf::from(OsString::from_wide(original_parent.as_wide()));
            let full_name = properties.GetString(&PKEY_FileName)?;
            let name = OsString::from_wide(full_name.as_wide());
            CoTaskMemFree(Some(full_name.0.cast()));
            ensure!(
                Path::new(&name).file_name() == Some(name.as_os_str()),
                "Invalid Recycle Bin filename"
            );
            let parsing = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?;
            let id = parsing.to_string();
            CoTaskMemFree(Some(parsing.0.cast()));
            entries.push(BinEntry {
                id: id?,
                original: parent.join(name),
                item,
            });
        }
        Ok(entries)
    }
}

fn verified_item(path: &Path) -> Result<(IShellItem, PathBuf)> {
    let name = path.file_name().context("Cannot recycle a drive root")?;
    let source = path
        .parent()
        .context("Missing parent")?
        .canonicalize()?
        .join(name);
    let plain = plain_drive_path(&source)?;
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(plain.as_os_str()), None)? };
    let parsed = item_path(&item)?;
    ensure!(
        source.file_name() == parsed.file_name()
            && identity(&source)? == identity(&parsed)?
            && identity(source.parent().context("Missing source parent")?)?
                == identity(parsed.parent().context("Missing Shell parent")?)?,
        "The Windows Shell resolved a different directory entry; nothing was recycled"
    );
    Ok((item, plain))
}

type Identity = (u32, u32, u32);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    bin_id: String,
    nonce: uuid::Uuid,
    original: PathBuf,
    identity: Identity,
    parent_identity: Identity,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    receipt: Receipt,
    directory_identity: Identity,
}

impl Receipt {
    fn parse(text: &str) -> Result<Self> {
        serde_json::from_str(text)
            .context("Legacy Recycle Bin receipt: restore this item manually from Windows")
    }
    fn parent(&self) -> Result<&Path> {
        self.original
            .parent()
            .context("Receipt has no original parent")
    }
    fn stage(&self) -> Result<PathBuf> {
        Ok(self
            .parent()?
            .join(format!(".rawtonic-restore-{}", self.nonce)))
    }
    fn bin_item(&self) -> Result<Option<IShellItem>> {
        let Some(found) = bin_entries()?
            .into_iter()
            .find(|entry| entry.id == self.bin_id)
        else {
            return Ok(None);
        };
        ensure!(
            plain_drive_path(&found.original)? == plain_drive_path(&self.original)?,
            "Recycle Bin receipt names another original"
        );
        ensure!(
            identity(&item_path(&found.item)?)? == self.identity,
            "Recycle Bin item identity changed"
        );
        Ok(Some(found.item))
    }
}

// Held without delete sharing, so neither the stage nor its parent can be
// replaced with a junction while a Shell operation uses its path.
fn open_directory(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(3)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & 0x400 == 0,
        "Restore directory is a reparse point or not a directory"
    );
    Ok(file)
}

struct Stage {
    directory: File,
    marker: File,
    path: PathBuf,
}
impl Stage {
    fn open(receipt: &Receipt, create: bool) -> Result<Self> {
        let path = receipt.stage()?;
        let created = if create {
            match fs::create_dir(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(e) => return Err(e.into()),
            }
        } else {
            false
        };
        let directory = open_directory(&path)?;
        let directory_identity = file_identity(&directory)?;
        let marker_path = path.join("receipt.json");
        // A crash can leave mkdir committed before the atomic marker write.
        // Adopt only an empty, pinned plain directory at this receipt's nonce;
        // never claim a payload or an unexpected entry without its marker.
        let initialize = created
            || (create
                && !marker_path.try_exists()?
                && fs::read_dir(&path)?.next().transpose()?.is_none());
        if initialize {
            let mut temp = tempfile::NamedTempFile::new_in(&path)?;
            serde_json::to_writer(
                &mut temp,
                &Marker {
                    receipt: receipt.clone(),
                    directory_identity,
                },
            )?;
            temp.flush()?;
            temp.as_file().sync_all()?;
            rawtonic_metadata::no_clobber::persist_in_own_folder(temp, &marker_path)?;
        }
        let mut marker = OpenOptions::new()
            .read(true)
            .access_mode(0x8000_0000 | 0x0001_0000)
            .share_mode(1)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(marker_path)?;
        ensure!(
            marker.metadata()?.file_attributes() & 0x400 == 0,
            "Restore marker is a reparse point"
        );
        let mut bytes = Vec::new();
        Read::by_ref(&mut marker)
            .take(64 * 1024)
            .read_to_end(&mut bytes)?;
        let data: Marker = serde_json::from_slice(&bytes)?;
        ensure!(
            data.receipt == *receipt && data.directory_identity == directory_identity,
            "Restore stage ownership changed"
        );
        for entry in fs::read_dir(&path)? {
            let name = entry?.file_name();
            ensure!(
                name == "receipt.json" || name == "payload",
                "Restore stage contains an unexpected entry"
            );
        }
        Ok(Self {
            directory,
            marker,
            path,
        })
    }
    fn payload(&self) -> PathBuf {
        self.path.join("payload")
    }
    fn cleanup(self) -> Result<()> {
        // Delete the proven-owned open handles, never a replacement at a path.
        ensure!(
            fs::read_dir(&self.path)?.count() == 1,
            "Restore stage is not empty"
        );
        let info = FILE_DISPOSITION_INFO {
            DeleteFile: true.into(),
        };
        unsafe {
            SetFileInformationByHandle(
                HANDLE(self.marker.as_raw_handle() as isize),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&info) as u32,
            )?;
        }
        drop(self.marker);
        unsafe {
            SetFileInformationByHandle(
                HANDLE(self.directory.as_raw_handle() as isize),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&info) as u32,
            )?;
        }
        Ok(())
    }
}

fn restore_to_stage(receipt: &Receipt, stage: &Stage) -> Result<()> {
    if fs::symlink_metadata(stage.payload()).is_ok() {
        ensure!(
            identity(&stage.payload())? == receipt.identity,
            "Staged entry identity changed"
        );
        ensure!(
            receipt.bin_item()?.is_none(),
            "Both staged and bin entries exist"
        );
        return Ok(());
    }
    let item = receipt
        .bin_item()?
        .context("Neither bin nor staged entry is available")?;
    let (folder, _) = verified_item(&stage.path)?;
    ensure!(
        identity(&item_path(&folder)?)? == file_identity(&stage.directory)?,
        "Shell resolved another staging directory"
    );
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS | FOFX_EARLYFAILURE)?;
        // The directory is private, ownership-checked and held open. Only this
        // operation creates payload; no existing destination is ever accepted.
        operation.MoveItem(&item, &folder, &HSTRING::from("payload"), None)?;
        operation.PerformOperations()?;
        ensure!(
            !operation.GetAnyOperationsAborted()?.as_bool(),
            "Staged restore was aborted"
        );
    }
    ensure!(
        identity(&stage.payload())? == receipt.identity,
        "Restored entry identity changed"
    );
    Ok(())
}

fn restore_receipt(receipt: &Receipt, original: &Path) -> Result<()> {
    ensure!(
        original == receipt.original,
        "Receipt belongs to another destination"
    );
    let parent = open_directory(receipt.parent()?)?;
    ensure!(
        file_identity(&parent)? == receipt.parent_identity,
        "Restore parent identity changed"
    );
    if fs::symlink_metadata(original).is_ok() {
        // Idempotent after the final rename, before the journal records success.
        ensure!(
            identity(original)? == receipt.identity && receipt.bin_item()?.is_none(),
            "Restore destination is occupied; item retained"
        );
        if receipt.stage()?.exists() {
            let stage = Stage::open(receipt, true)?;
            ensure!(
                !stage.payload().exists(),
                "Both destination and stage are occupied"
            );
            if let Err(error) = stage.cleanup() {
                log::warn!("Restored entry verified; owned staging cleanup remains: {error:#}");
            }
        }
        return Ok(());
    }
    let stage = Stage::open(receipt, true)?;
    restore_to_stage(receipt, &stage)?;
    rawtonic_fileops::rename_no_clobber(&stage.payload(), original).context(
        "Restore destination became occupied; item remains in its recoverable staging directory",
    )?;
    // The original has been restored at this point. A cleanup failure must
    // not make the journal claim that the undo failed after its file moved.
    if let Err(error) = stage.cleanup() {
        log::warn!("Entry restored; owned staging cleanup remains: {error:#}");
    }
    Ok(())
}

impl SystemTrash for PlatformTrash {
    fn trash(&self, path: &Path) -> Result<String> {
        let _apartment = Apartment::new()?;
        let (item, plain) = verified_item(path)?;
        let original = path
            .parent()
            .context("Missing parent")?
            .canonicalize()?
            .join(path.file_name().context("Missing filename")?);
        let mut receipt = Receipt {
            bin_id: String::new(),
            nonce: uuid::Uuid::new_v4(),
            identity: identity(&original)?,
            parent_identity: identity(original.parent().unwrap())?,
            original,
        };
        // Check serialization before any mutation (including non-Unicode paths).
        serde_json::to_string(&receipt)?;
        let before: HashSet<_> = bin_entries()?.into_iter().map(|i| i.id).collect();
        unsafe {
            let operation: IFileOperation =
                CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
            // Windows 8+: require recycling, with no connected-file expansion.
            // Unlike FOF_ALLOWUNDO this requests recycle rather than deletion.
            operation.SetOperationFlags(
                FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS | FOFX_EARLYFAILURE | FOFX_RECYCLEONDELETE,
            )?;
            operation.DeleteItem(&item, None)?;
            operation.PerformOperations()?;
            ensure!(
                !operation.GetAnyOperationsAborted()?.as_bool(),
                "Recycling was aborted"
            );
        }
        let mut matching = Vec::new();
        for entry in bin_entries()? {
            if !before.contains(&entry.id) && plain_drive_path(&entry.original)? == plain {
                matching.push(entry);
            }
        }
        ensure!(
            matching.len() == 1,
            "The recycled item's receipt was not uniquely identified"
        );
        let bin_entry = matching.pop().unwrap();
        receipt.bin_id = bin_entry.id;
        ensure!(
            receipt.bin_item()?.is_some(),
            "Recycled entry could not be verified"
        );
        Ok(serde_json::to_string(&receipt)?)
    }

    fn restore(&self, item: &str, original: &Path) -> Result<()> {
        let _apartment = Apartment::new()?;
        restore_receipt(&Receipt::parse(item)?, original)
    }

    fn restore_for_recovery(&self, item: &str, original: &Path) -> Result<()> {
        self.restore(item, original)
    }

    fn contains(&self, item: &str) -> bool {
        (|| -> Result<bool> {
            let _apartment = Apartment::new()?;
            let receipt = match Receipt::parse(item) {
                Ok(receipt) => receipt,
                // Keep old receipts visibly recoverable. restore() explains
                // that their identity cannot be proven and requires Windows.
                Err(_) => {
                    return Ok(trash::os_limited::list()?
                        .iter()
                        .any(|entry| entry.id.to_string_lossy() == item));
                }
            };
            if receipt.bin_item()?.is_some() {
                return Ok(true);
            }
            if receipt.stage()?.exists() {
                let stage = Stage::open(&receipt, false)?;
                if stage.payload().exists() {
                    return Ok(identity(&stage.payload())? == receipt.identity);
                }
            }
            Ok(false)
        })()
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn staged_fixture() -> (tempfile::TempDir, Receipt) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = root.join("photo.txt");
        fs::write(&original, b"synthetic photograph").unwrap();
        let receipt = Receipt {
            bin_id: format!("synthetic-unavailable-{}", uuid::Uuid::new_v4()),
            nonce: uuid::Uuid::new_v4(),
            identity: identity(&original).unwrap(),
            parent_identity: identity(&root).unwrap(),
            original,
        };
        let stage = Stage::open(&receipt, true).unwrap();
        rawtonic_fileops::rename_no_clobber(&receipt.original, &stage.payload()).unwrap();
        (temp, receipt)
    }

    #[test]
    fn empty_markerless_stage_can_resume_but_unmarked_payload_is_refused() {
        let (_temp, receipt) = staged_fixture();
        let path = receipt.stage().unwrap();
        rawtonic_fileops::rename_no_clobber(&path.join("payload"), &receipt.original).unwrap();
        fs::remove_file(path.join("receipt.json")).unwrap();
        fs::remove_dir(&path).unwrap();
        fs::create_dir(&path).unwrap(); // Simulated crash immediately after mkdir.
        assert!(Stage::open(&receipt, false).is_err());
        let stage = Stage::open(&receipt, true).unwrap();
        stage.cleanup().unwrap();
        assert!(!path.exists());

        fs::create_dir(&path).unwrap();
        fs::write(path.join("payload"), b"unrelated entry").unwrap();
        assert!(Stage::open(&receipt, true).is_err());
        assert_eq!(fs::read(path.join("payload")).unwrap(), b"unrelated entry");
        assert!(!path.join("receipt.json").exists());
    }

    #[test]
    fn staged_restore_preserves_collision_and_retries_with_same_receipt() {
        let _apartment = Apartment::new().unwrap();
        let (_temp, receipt) = staged_fixture();
        let text = serde_json::to_string(&receipt).unwrap();
        fs::write(&receipt.original, b"unrelated destination").unwrap();
        assert!(PlatformTrash.restore(&text, &receipt.original).is_err());
        assert_eq!(
            fs::read(&receipt.original).unwrap(),
            b"unrelated destination"
        );
        assert_eq!(
            fs::read(receipt.stage().unwrap().join("payload")).unwrap(),
            b"synthetic photograph"
        );
        assert!(PlatformTrash.contains(&text));
        fs::remove_file(&receipt.original).unwrap();
        PlatformTrash.restore(&text, &receipt.original).unwrap();
        assert_eq!(
            fs::read(&receipt.original).unwrap(),
            b"synthetic photograph"
        );
        assert!(!PlatformTrash.contains(&text));
        assert!(!receipt.stage().unwrap().exists());
    }

    #[test]
    fn restore_is_idempotent_after_final_rename_before_journal_commit() {
        let _apartment = Apartment::new().unwrap();
        let (_temp, receipt) = staged_fixture();
        rawtonic_fileops::rename_no_clobber(
            &receipt.stage().unwrap().join("payload"),
            &receipt.original,
        )
        .unwrap();
        let text = serde_json::to_string(&receipt).unwrap();
        PlatformTrash.restore(&text, &receipt.original).unwrap();
        PlatformTrash.restore(&text, &receipt.original).unwrap();
        assert_eq!(
            fs::read(&receipt.original).unwrap(),
            b"synthetic photograph"
        );
        assert!(!receipt.stage().unwrap().exists());
    }

    #[test]
    fn restore_refuses_substituted_stage_marker_and_junction() {
        let _apartment = Apartment::new().unwrap();
        let (_temp, receipt) = staged_fixture();
        let stage_path = receipt.stage().unwrap();
        let marker = fs::read(stage_path.join("receipt.json")).unwrap();
        fs::write(stage_path.join("receipt.json"), b"{}").unwrap();
        let text = serde_json::to_string(&receipt).unwrap();
        assert!(PlatformTrash.restore(&text, &receipt.original).is_err());
        fs::write(stage_path.join("receipt.json"), &marker).unwrap();
        let moved = receipt.parent().unwrap().join("retained-original-stage");
        fs::rename(&stage_path, &moved).unwrap();
        fs::create_dir(&stage_path).unwrap();
        fs::write(stage_path.join("receipt.json"), &marker).unwrap();
        assert!(PlatformTrash.restore(&text, &receipt.original).is_err());
        fs::remove_file(stage_path.join("receipt.json")).unwrap();
        fs::remove_dir(&stage_path).unwrap();
        std::os::windows::fs::symlink_dir(&moved, &stage_path).unwrap();
        assert!(PlatformTrash.restore(&text, &receipt.original).is_err());
        assert_eq!(
            fs::read(moved.join("payload")).unwrap(),
            b"synthetic photograph"
        );
        assert!(!receipt.original.exists());
    }

    /// Exercises the real system Bin with only a fresh synthetic file.
    #[test]
    #[ignore = "Uses the Windows Recycle Bin; run explicitly during native qualification"]
    fn platform_trash_journal_recovers_both_restore_crash_boundaries() {
        use rawtonic_fileops::{Journal, State, Step};
        use std::panic::{AssertUnwindSafe, catch_unwind};
        struct CrashAfterRestore {
            staged: bool,
        }
        impl SystemTrash for CrashAfterRestore {
            fn trash(&self, path: &Path) -> Result<String> {
                PlatformTrash.trash(path)
            }
            fn contains(&self, item: &str) -> bool {
                PlatformTrash.contains(item)
            }
            fn restore(&self, item: &str, original: &Path) -> Result<()> {
                if self.staged {
                    let _apartment = Apartment::new()?;
                    let receipt = Receipt::parse(item)?;
                    let stage = Stage::open(&receipt, true)?;
                    restore_to_stage(&receipt, &stage)?;
                } else {
                    PlatformTrash.restore(item, original)?;
                }
                panic!("simulated process interruption before journal commit");
            }
        }
        let before: HashSet<_> = trash::os_limited::list()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        for staged in [true, false] {
            let temp = match std::env::var_os("RAWTONIC_TEST_TRASH_DIR") {
                Some(directory) => tempfile::tempdir_in(directory).unwrap(),
                None => tempfile::tempdir().unwrap(),
            };
            let root = temp.path().canonicalize().unwrap();
            let original = root.join("rawtonic-recovery-probe.txt");
            fs::write(&original, b"synthetic photograph").unwrap();
            let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
            let operation = journal
                .execute(
                    "trash",
                    "Trash synthetic probe",
                    vec![Step::system_trash(original.clone())],
                    &PlatformTrash,
                )
                .unwrap();
            assert!(
                catch_unwind(AssertUnwindSafe(
                    || journal.undo(&operation.id, &CrashAfterRestore { staged })
                ))
                .is_err()
            );
            assert_eq!(
                journal.operation(&operation.id).unwrap().state,
                State::Undoing
            );
            if staged {
                fs::write(&original, b"unrelated destination").unwrap();
                assert!(journal.recover(&PlatformTrash).unwrap().is_empty());
                assert_eq!(
                    journal.operation(&operation.id).unwrap().state,
                    State::Undoing
                );
                assert_eq!(fs::read(&original).unwrap(), b"unrelated destination");
                fs::remove_file(&original).unwrap();
            }
            journal.recover(&PlatformTrash).unwrap();
            assert_eq!(
                journal.operation(&operation.id).unwrap().state,
                State::Undone
            );
            assert_eq!(fs::read(&original).unwrap(), b"synthetic photograph");
        }
        let after: HashSet<_> = trash::os_limited::list()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(
            before, after,
            "Qualification must leave unrelated bin entries unchanged"
        );
    }

    #[test]
    fn shell_entry_accepts_a_canonical_file() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.txt");
        fs::write(&source, b"synthetic").unwrap();
        let (_, plain) = verified_item(&source.canonicalize().unwrap()).unwrap();
        assert_eq!(plain, source);
        assert_eq!(fs::read(&source).unwrap(), b"synthetic");
    }

    #[test]
    fn shell_entry_refuses_literal_names_that_resolve_to_another_entry() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let literal = root.join("photo.txt.");
        let sibling = root.join("photo.txt");
        fs::write(&literal, b"literal").unwrap();
        fs::write(&sibling, b"sibling").unwrap();
        assert!(verified_item(&literal).is_err());
        assert_eq!(fs::read(&literal).unwrap(), b"literal");
        assert_eq!(fs::read(&sibling).unwrap(), b"sibling");
    }

    #[test]
    fn shell_entry_refuses_normalized_parent_and_link_aliases() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let literal = root.join("album ");
        let sibling = root.join("album");
        fs::create_dir(&literal).unwrap();
        fs::create_dir(&sibling).unwrap();
        let source = literal.join("photo.txt");
        let alias = sibling.join("photo.txt");
        fs::write(&source, b"synthetic").unwrap();
        fs::hard_link(&source, &alias).unwrap();
        assert!(verified_item(&source).is_err());
        fs::remove_file(&alias).unwrap();
        std::os::windows::fs::symlink_file(&source, &alias).unwrap();
        assert!(verified_item(&source).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"synthetic");
        assert!(fs::symlink_metadata(&alias).unwrap().is_symlink());
    }
}

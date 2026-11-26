// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Shell uses ordinary paths; the library retains verbatim native paths.
//! Verify the entry the Shell parsed before allowing a recycle operation.
use super::PlatformTrash;
use super::entry_proof::{EntryFacts, Identity, Stamp, same_entry, stamp_to_record};
use anyhow::{Context, Result, ensure};
use rawtonic_fileops::SystemTrash;
use serde::{Deserialize, Serialize};
use std::{
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
        Foundation::{FILETIME, HANDLE},
        Storage::EnhancedStorage::PKEY_FileName,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TEMPORARY, FILE_DISPOSITION_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT,
            FileDispositionInfo, GetFileInformationByHandle, GetVolumeInformationByHandleW,
            SetFileInformationByHandle,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize, StructuredStorage::PropVariantToBSTR,
        },
        UI::Shell::{
            BHID_EnumItems, BHID_Transfer, FOF_NO_CONNECTED_ELEMENTS, FOF_NO_UI, FOFX_EARLYFAILURE,
            FOLDERID_RecycleBinFolder, FileOperation, IEnumShellItems, IFileOperation, IShellItem,
            IShellItem2, ITransferSource, KF_FLAG_DEFAULT, PID_DISPLACED_FROM, PSGUID_DISPLACED,
            PropertiesSystem::PROPERTYKEY, SHCreateItemFromParsingName, SHGetKnownFolderItem,
            SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
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

// Shell objects stay on a fresh STA thread, never a reused Tauri pool
// thread that another library may already have initialized as MTA.
fn shell_thread<T: Send>(job: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(job)
            .join()
            .map_err(|_| anyhow::anyhow!("Windows Shell worker panicked"))?
    })
}

fn desktop_id(item: &IShellItem) -> Result<String> {
    unsafe {
        let text = item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?;
        let value = text.to_string();
        CoTaskMemFree(Some(text.0.cast()));
        Ok(value?)
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

/// The entry's facts for [`same_entry`] and its volume's file system name.
fn entry_facts(path: &Path) -> Result<(EntryFacts, String)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    let handle = HANDLE(file.as_raw_handle() as isize);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let mut name = [0u16; 64];
    unsafe {
        GetFileInformationByHandle(handle, &mut info)?;
        GetVolumeInformationByHandleW(handle, None, None, None, None, Some(&mut name))?;
    }
    let time = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    let facts = EntryFacts {
        identity: (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ),
        stamp: Stamp {
            size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
            created: time(info.ftCreationTime),
            written: time(info.ftLastWriteTime),
            directory: info.dwFileAttributes & 0x10 != 0,
        },
    };
    Ok((facts, String::from_utf16_lossy(&name[..end])))
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
            let id = desktop_id(&item)?;
            entries.push(BinEntry {
                id,
                original: parent.join(name),
                item,
            });
        }
        Ok(entries)
    }
}

/// Verbatim on purpose: the entry's own name is joined literally, so
/// `photo.txt.` stays itself instead of becoming its sibling `photo.txt`
/// (a plain parent would let the identity check pass on the wrong file).
fn canonical_entry(path: &Path) -> Result<PathBuf> {
    #[allow(clippy::disallowed_methods)]
    let parent = fs::canonicalize(path.parent().context("Missing parent")?)?;
    Ok(parent.join(path.file_name().context("Missing filename")?))
}

fn verified_item(path: &Path) -> Result<(IShellItem, PathBuf)> {
    path.file_name().context("Cannot recycle a drive root")?;
    let source = canonical_entry(path)?;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    bin_id: String,
    nonce: uuid::Uuid,
    original: PathBuf,
    identity: Identity,
    parent_identity: Identity,
    /// Set on volumes without file IDs (exFAT, FAT32), where the entry's
    /// record, not its index, proves it after a move (`entry_proof`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<Stamp>,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    receipt: Receipt,
    directory_identity: Identity,
}

fn write_stage_marker(path: &Path, marker_path: &Path, data: &Marker) -> Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(".marker-")
        .make_in(path, |candidate| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .access_mode(0xC001_0000)
                .share_mode(7)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE.0 | FILE_ATTRIBUTE_TEMPORARY.0)
                .open(candidate)
        })?;
    #[cfg(test)]
    marker_crash_point("partial", temp.as_file())?;
    serde_json::to_writer(&mut temp, data)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    // A separate open (not try_clone) survives closure of the delete-on-close
    // file object. Until publication, every process-exit path deletes the temp.
    let keeper = OpenOptions::new()
        .read(true)
        .access_mode(0x8001_0000)
        .share_mode(7)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(temp.path())?;
    ensure!(
        file_identity(&keeper)? == file_identity(temp.as_file())?,
        "Marker identity changed"
    );
    rawtonic_fs::no_clobber::persist_in_own_folder(temp, marker_path)?;
    #[cfg(test)]
    marker_crash_point("published", &keeper)?;
    // persist closed the original file object. Cancel its pending deletion on
    // the separately opened object; FILE_FLAG_DELETE_ON_CLOSE itself is permanent.
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: false.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(keeper.as_raw_handle() as isize),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )?;
    }
    Ok(())
}

#[cfg(test)]
fn marker_crash_point(phase: &str, file: &File) -> Result<()> {
    if std::env::var("RAWTONIC_MARKER_CRASH_PHASE").ok().as_deref() != Some(phase) {
        return Ok(());
    }
    if phase == "partial" {
        let mut writer = file;
        writer.write_all(b"{")?;
        file.sync_all()?;
    }
    println!("RAWTONIC_MARKER_READY");
    std::io::stdout().flush()?;
    loop {
        std::thread::park();
    }
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
    /// Whether `path` is this receipt's entry (it may have moved since).
    fn is_entry(&self, path: &Path) -> Result<bool> {
        Ok(self.matches(&entry_facts(path)?.0))
    }
    fn matches(&self, facts: &EntryFacts) -> bool {
        same_entry(self.identity, self.stamp.as_ref(), facts)
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
            self.is_entry(&item_path(&found.item)?)?,
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
            write_stage_marker(
                &path,
                &marker_path,
                &Marker {
                    receipt: receipt.clone(),
                    directory_identity,
                },
            )?;
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

/// Moves the receipt's Bin entry into the stage; returns the entry's own
/// place in the Bin (its `$R` file) when this call moved it.
fn restore_to_stage(receipt: &Receipt, stage: &Stage) -> Result<Option<PathBuf>> {
    if fs::symlink_metadata(stage.payload()).is_ok() {
        ensure!(
            receipt.is_entry(&stage.payload())?,
            "Staged entry identity changed"
        );
        ensure!(
            receipt.bin_item()?.is_none(),
            "Both staged and bin entries exist"
        );
        return Ok(None);
    }
    let item = receipt
        .bin_item()?
        .context("Neither bin nor staged entry is available")?;
    let data = item_path(&item)?;
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
        receipt.is_entry(&stage.payload())?,
        "Restored entry identity changed"
    );
    Ok(Some(data))
}

/// The Shell moves only the `$R` data entry out of the Bin; its `$I` info
/// stub stays behind, unlisted, for good (WQA7: notes
/// benchmark-results/wqa7-2026-11-26/REPORT.md). After a verified restore
/// this removes that stub — only the `$I` sibling of the entry restored,
/// only once the entry is gone from the Bin and back at the receipt's
/// original with its identity, and only when the stub records that
/// original (and, for a file, its size). Anything else stays; a stub left
/// is no loss. `data` is the entry's `$R` path, or `None` when an earlier
/// attempt moved it (then the receipt's Bin id names it).
fn remove_info_stub(data: Option<&Path>, receipt: &Receipt) -> Result<bool> {
    use super::recycle_info::{MAX_STUB_LEN, info_stub_for, stub_describes};
    let data = data.map_or_else(|| PathBuf::from(&receipt.bin_id), Path::to_path_buf);
    let Some(stub) = info_stub_for(&data) else {
        return Ok(false);
    };
    ensure!(
        matches!(fs::symlink_metadata(&data), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "The Bin entry is still there"
    );
    ensure!(
        receipt.is_entry(&receipt.original)?,
        "The restored entry identity changed"
    );
    let restored = fs::symlink_metadata(&receipt.original)?;
    // Read through the handle that deletes it, shared for reading only, so
    // the bytes checked are the ones removed.
    let mut file = match OpenOptions::new()
        .read(true)
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(1)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(&stub)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.file_attributes() & 0x400 == 0
            && metadata.len() <= MAX_STUB_LEN,
        "Not a Recycle Bin info stub"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_STUB_LEN + 1)
        .read_to_end(&mut bytes)?;
    let original = plain_drive_path(&receipt.original)?;
    let size = (!restored.is_dir()).then_some(restored.len());
    ensure!(
        original
            .to_str()
            .is_some_and(|original| stub_describes(&bytes, original, size)),
        "The info stub describes another entry"
    );
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: true.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle() as isize),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )?;
    }
    Ok(true)
}

/// `remove_info_stub` after the restore succeeded: a stub that cannot be
/// proven the entry's own stays, with a warning, and never fails the undo.
fn settle_info_stub(data: Option<&Path>, receipt: &Receipt) {
    if let Err(error) = remove_info_stub(data, receipt) {
        log::warn!(
            "Entry restored; its Recycle Bin info stub stays ({}): {error:#}",
            receipt.bin_id
        );
    }
}

fn restore_receipt(receipt: &Receipt, original: &Path) -> Result<()> {
    // Both sides through one key: a receipt may record the plain or the
    // verbatim spelling of its folder (the caller's may differ again), and
    // canonical_entry maps both to one verbatim parent while keeping the
    // entry's own name literal, so `photo.txt.` never matches `photo.txt`.
    let recorded =
        canonical_entry(&receipt.original).context("The receipt's original folder is gone")?;
    ensure!(
        canonical_entry(original)? == recorded,
        "Receipt belongs to another destination"
    );
    // Use the recorded native spelling after checking the caller's parent and
    // leaf, so plain and verbatim aliases have the same no-clobber guarantees.
    let original = receipt.original.as_path();
    let parent = open_directory(receipt.parent()?)?;
    ensure!(
        file_identity(&parent)? == receipt.parent_identity,
        "Restore parent identity changed"
    );
    if fs::symlink_metadata(original).is_ok() {
        // Idempotent after the final rename, before the journal records success.
        ensure!(
            receipt.is_entry(original)? && receipt.bin_item()?.is_none(),
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
        settle_info_stub(None, receipt);
        return Ok(());
    }
    let stage = Stage::open(receipt, true)?;
    let data = restore_to_stage(receipt, &stage)?;
    rawtonic_fs::rename_no_clobber(&stage.payload(), original).context(
        "Restore destination became occupied; item remains in its recoverable staging directory",
    )?;
    // The original has been restored at this point. A cleanup failure must
    // not make the journal claim that the undo failed after its file moved.
    if let Err(error) = stage.cleanup() {
        log::warn!("Entry restored; owned staging cleanup remains: {error:#}");
    }
    settle_info_stub(data.as_deref(), receipt);
    Ok(())
}

/// Proves that `location`, where the Shell says it recycled the receipt's
/// entry, holds that entry. First by the receipt's own proof (file ID, or
/// on exFAT/FAT32 the record's size and times). Failing that, the entry is
/// still taken as recycled when it has left its original place and its Bin
/// record (`$I`) names that original and its size: the receipt then keeps
/// the facts the entry has now, so Undo can put it back. Anything else is
/// an error, and the journal reports that the entry left its folder.
fn prove_recycled(receipt: &mut Receipt, before: &EntryFacts, location: &Path) -> Result<()> {
    use super::recycle_info::{MAX_STUB_LEN, info_stub_for, stub_describes};
    let (now, file_system) = entry_facts(location)?;
    if receipt.matches(&now) {
        return Ok(());
    }
    // On NTFS and ReFS a different file ID is a different file: never
    // adopted. On FAT-family volumes the entry itself must still be the one
    // recycled: same kind and, for a file, same size.
    ensure!(
        receipt.stamp.is_some()
            && now.stamp.directory == before.stamp.directory
            && (now.stamp.directory || now.stamp.size == before.stamp.size),
        "The entry in the Recycle Bin is not the one recycled"
    );
    ensure!(
        matches!(fs::symlink_metadata(&receipt.original), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "The original place is not empty"
    );
    let stub = info_stub_for(location).context("The new entry is no Recycle Bin entry")?;
    let mut bytes = Vec::new();
    File::open(&stub)
        .with_context(|| format!("Its Recycle Bin record {} cannot be read", stub.display()))?
        .take(MAX_STUB_LEN + 1)
        .read_to_end(&mut bytes)?;
    let original = plain_drive_path(&receipt.original)?;
    let size = (!before.stamp.directory).then_some(before.stamp.size);
    ensure!(
        bytes.len() as u64 <= MAX_STUB_LEN
            && original
                .to_str()
                .is_some_and(|original| stub_describes(&bytes, original, size)),
        "Its Recycle Bin record names another entry"
    );
    log::warn!(
        "Recycled {} to {}: proven by its Recycle Bin record; its {} entry facts changed in the move",
        receipt.original.display(),
        location.display(),
        file_system
    );
    receipt.identity = now.identity;
    receipt.stamp = stamp_to_record(&file_system, &now);
    Ok(())
}

impl SystemTrash for PlatformTrash {
    fn trash(&self, path: &Path) -> Result<String> {
        shell_thread(|| {
            let _apartment = Apartment::new()?;
            let (item, _) = verified_item(path)?;
            let original = canonical_entry(path)?;
            let (before, file_system) = entry_facts(&original)?;
            let mut receipt = Receipt {
                bin_id: String::new(),
                nonce: uuid::Uuid::new_v4(),
                identity: before.identity,
                parent_identity: identity(original.parent().unwrap())?,
                stamp: stamp_to_record(&file_system, &before),
                original,
            };
            // RecycleItem has no permanent-delete fallback. DeleteItem with recycle
            // flags still permanently deletes files on media without a Bin.
            serde_json::to_string(&receipt)?;
            unsafe {
                let parent = item.GetParent()?;
                ensure!(
                    identity(&item_path(&parent)?)? == receipt.parent_identity,
                    "Shell resolved another source parent"
                );
                let source: ITransferSource = parent.BindToHandler(None, &BHID_Transfer)?;
                let bin: IShellItem = SHGetKnownFolderItem(
                    &FOLDERID_RecycleBinFolder,
                    KF_FLAG_DEFAULT,
                    HANDLE::default(),
                )?;
                let recycled = source.RecycleItem(&item, &bin, 0).context(
                    "Windows cannot recycle this item; permanent deletion is not supported",
                )?;
                receipt.bin_id = desktop_id(&recycled)?;
                let location = item_path(&recycled)?;
                prove_recycled(&mut receipt, &before, &location).with_context(|| {
                    format!(
                        "Windows moved it to {}, but could not prove it is the same entry",
                        location.display()
                    )
                })?;
            }
            Ok(serde_json::to_string(&receipt)?)
        })
    }

    fn restore(&self, item: &str, original: &Path) -> Result<()> {
        shell_thread(|| {
            let _apartment = Apartment::new()?;
            restore_receipt(&Receipt::parse(item)?, original)
        })
    }

    fn restore_for_recovery(&self, item: &str, original: &Path) -> Result<()> {
        self.restore(item, original)
    }

    fn contains(&self, item: &str) -> bool {
        shell_thread(|| {
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
            let _apartment = Apartment::new()?;
            if receipt.bin_item()?.is_some() {
                return Ok(true);
            }
            if receipt.stage()?.exists() {
                let stage = Stage::open(&receipt, false)?;
                if stage.payload().exists() {
                    return receipt.is_entry(&stage.payload());
                }
            }
            Ok(false)
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_fs::Canonical;
    use std::{collections::HashSet, fs};

    /// Symbolic links need Developer Mode or the symlink privilege; without
    /// it (ERROR_PRIVILEGE_NOT_HELD) the rest of the test is skipped, with
    /// SKIP printed (as in rawtonic-fileops' copy/move tests).
    fn link_or_skip(made: std::io::Result<()>) -> bool {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        match made {
            Ok(()) => true,
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
                eprintln!(
                    "SKIP: no right to make symbolic links ({e}); the link checks did not run"
                );
                false
            }
            Err(e) => panic!("cannot make a symbolic link: {e}"),
        }
    }

    fn staged_fixture() -> (tempfile::TempDir, Receipt) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonical().unwrap();
        staged_fixture_at(temp, &root, "photo.txt")
    }

    /// A synthetic file at `root\name`, moved into its restore stage under a
    /// receipt that records `root` in the spelling it is given.
    fn staged_fixture_at(
        temp: tempfile::TempDir,
        root: &Path,
        name: &str,
    ) -> (tempfile::TempDir, Receipt) {
        let original = root.join(name);
        fs::write(&original, b"synthetic photograph").unwrap();
        let receipt = Receipt {
            bin_id: format!("synthetic-unavailable-{}", uuid::Uuid::new_v4()),
            nonce: uuid::Uuid::new_v4(),
            identity: identity(&original).unwrap(),
            parent_identity: identity(root).unwrap(),
            stamp: None,
            original,
        };
        let stage = Stage::open(&receipt, true).unwrap();
        rawtonic_fs::rename_no_clobber(&receipt.original, &stage.payload()).unwrap();
        (temp, receipt)
    }

    #[test]
    fn empty_markerless_stage_can_resume_but_unmarked_payload_is_refused() {
        let (_temp, receipt) = staged_fixture();
        let path = receipt.stage().unwrap();
        rawtonic_fs::rename_no_clobber(&path.join("payload"), &receipt.original).unwrap();
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
    fn marker_process_kill_leaves_no_partial_or_published_orphan() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
            sync::mpsc,
            time::Duration,
        };
        if std::env::var_os("RAWTONIC_MARKER_CRASH_PHASE").is_some() {
            let receipt: Receipt =
                serde_json::from_str(&std::env::var("RAWTONIC_MARKER_RECEIPT").unwrap()).unwrap();
            Stage::open(&receipt, true).unwrap();
            panic!("Marker child did not reach its interruption point");
        }
        for phase in ["partial", "published"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonical().unwrap();
            let original = root.join("synthetic-original.txt");
            fs::write(&original, b"untouched synthetic bytes").unwrap();
            let receipt = Receipt {
                bin_id: "synthetic-marker-only".into(),
                nonce: uuid::Uuid::new_v4(),
                identity: identity(&original).unwrap(),
                parent_identity: identity(&root).unwrap(),
                stamp: None,
                original,
            };
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "file_operations::windows_trash::tests::marker_process_kill_leaves_no_partial_or_published_orphan", "--nocapture"])
                .env("RAWTONIC_MARKER_CRASH_PHASE", phase)
                .env("RAWTONIC_MARKER_RECEIPT", serde_json::to_string(&receipt).unwrap())
                .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (sender, ready) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if line.contains("RAWTONIC_MARKER_READY") {
                        let _ = sender.send(());
                        break;
                    }
                }
            });
            let reached = ready.recv_timeout(Duration::from_secs(10));
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            reader.join().unwrap();
            assert!(
                reached.is_ok(),
                "Marker child failed before {phase}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.status.success());
            let path = receipt.stage().unwrap();
            assert_eq!(
                fs::read_dir(&path).unwrap().count(),
                0,
                "orphan after {phase}"
            );
            assert_eq!(
                fs::read(&receipt.original).unwrap(),
                b"untouched synthetic bytes"
            );
            Stage::open(&receipt, true).unwrap().cleanup().unwrap();
            assert!(!path.exists());
        }
    }

    /// Set `RAWTONIC_TEST_NO_BIN_DIR` to an existing writable folder on a
    /// disposable Windows volume without a Recycle Bin before running this test.
    #[test]
    #[ignore = "Requires a fresh synthetic removable volume with no Recycle Bin"]
    fn platform_trash_refuses_a_volume_without_a_bin_and_preserves_its_file() {
        let directory = std::env::var_os("RAWTONIC_TEST_NO_BIN_DIR")
            .filter(|directory| !directory.is_empty())
            .expect("Set RAWTONIC_TEST_NO_BIN_DIR to a writable folder on a disposable volume without a Recycle Bin");
        let temp = tempfile::tempdir_in(directory).unwrap();
        let source = temp.path().join("synthetic-original.txt");
        fs::write(&source, b"synthetic no-bin photograph").unwrap();
        let result = PlatformTrash.trash(&source);
        let error = match result {
            Ok(receipt) => {
                PlatformTrash.restore(&receipt, &source).unwrap();
                panic!("Test volume unexpectedly supports recycling");
            }
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("Windows cannot recycle this item")
        );
        assert_eq!(fs::read(&source).unwrap(), b"synthetic no-bin photograph");
    }

    /// A synthetic Bin folder (`<tmp>\$Recycle.Bin\<SID>`) holding the `$I`
    /// stub `bytes` of an entry restored to `<tmp>\Shoot\photo.NEF`, whose
    /// `$R` file is gone; the receipt names that entry.
    fn restored_with_stub(stub: impl FnOnce(&str, u64) -> Vec<u8>) -> (tempfile::TempDir, Receipt) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonical().unwrap();
        let bin = root.join("$Recycle.Bin").join("S-1-5-21-1-2-3-1001");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.join("Shoot")).unwrap();
        let original = root.join("Shoot").join("photo.NEF");
        fs::write(&original, b"synthetic photograph").unwrap();
        let plain = plain_drive_path(&original).unwrap();
        fs::write(bin.join("$IAB12CD.NEF"), stub(plain.to_str().unwrap(), 20)).unwrap();
        let receipt = Receipt {
            bin_id: plain_drive_path(&bin.join("$RAB12CD.NEF"))
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            nonce: uuid::Uuid::new_v4(),
            identity: identity(&original).unwrap(),
            parent_identity: identity(&root.join("Shoot")).unwrap(),
            stamp: None,
            original,
        };
        (temp, receipt)
    }

    fn stub_of(receipt: &Receipt) -> PathBuf {
        super::super::recycle_info::info_stub_for(Path::new(&receipt.bin_id)).unwrap()
    }

    #[test]
    fn a_restored_entrys_own_info_stub_is_removed() {
        use super::super::recycle_info::v2_stub;
        let (_temp, receipt) = restored_with_stub(|path, size| v2_stub(size, path));
        assert!(stub_of(&receipt).exists());
        assert!(remove_info_stub(None, &receipt).unwrap());
        assert!(!stub_of(&receipt).exists());
        assert_eq!(
            fs::read(&receipt.original).unwrap(),
            b"synthetic photograph"
        );
        // Again (a retried restore): nothing left to remove.
        assert!(!remove_info_stub(None, &receipt).unwrap());
    }

    #[test]
    fn an_info_stub_not_proven_the_entrys_own_stays() {
        use super::super::recycle_info::v2_stub;
        // Another original, another size.
        let cases: [fn(&str, u64) -> Vec<u8>; 2] = [
            |_, size| v2_stub(size, r"C:\Elsewhere\photo.NEF"),
            |path, size| v2_stub(size + 1, path),
        ];
        for stub in cases {
            let (_temp, receipt) = restored_with_stub(stub);
            assert!(remove_info_stub(None, &receipt).is_err());
            assert!(stub_of(&receipt).exists());
        }
        // Its `$R` entry is still in the Bin.
        let (_temp, receipt) = restored_with_stub(|path, size| v2_stub(size, path));
        fs::write(&receipt.bin_id, b"still recycled").unwrap();
        assert!(remove_info_stub(None, &receipt).is_err());
        assert!(stub_of(&receipt).exists());
        // The original is not the restored entry any more.
        let (_temp, mut receipt) = restored_with_stub(|path, size| v2_stub(size, path));
        receipt.identity.2 ^= 1;
        assert!(remove_info_stub(None, &receipt).is_err());
        assert!(stub_of(&receipt).exists());
        // A Bin id that names no `$R` entry has no stub.
        let (_temp, mut receipt) = restored_with_stub(|path, size| v2_stub(size, path));
        let stub = stub_of(&receipt);
        receipt.bin_id = "::{645FF040-5081-101B-9F08-00AA002F954E}".into();
        assert!(!remove_info_stub(None, &receipt).unwrap());
        assert!(stub.exists());
    }

    /// Trash and restore of a fresh synthetic file in `directory`: the
    /// entry comes back with its bytes, the Bin keeps neither its `$R` data
    /// nor its `$I` stub, and the receipt carries a record stamp exactly
    /// where the volume keeps no file IDs.
    fn round_trip_in(directory: &Path) {
        let temp = tempfile::tempdir_in(directory).unwrap();
        let original = temp
            .path()
            .canonical()
            .unwrap()
            .join("synthetic-stub-probe.txt");
        fs::write(&original, b"synthetic stub photograph").unwrap();
        let (_, file_system) = entry_facts(&original).unwrap();
        let item = PlatformTrash.trash(&original).unwrap();
        assert!(!original.exists());
        let receipt = Receipt::parse(&item).unwrap();
        assert_eq!(
            receipt.stamp.is_some(),
            !super::super::entry_proof::keeps_file_ids(&file_system),
            "{file_system}"
        );
        let data = shell_thread(|| {
            let _apartment = Apartment::new()?;
            item_path(&receipt.bin_item()?.context("not in the Bin")?)
        })
        .unwrap();
        let stub = super::super::recycle_info::info_stub_for(&data).unwrap();
        assert!(stub.exists(), "{}", stub.display());
        assert!(PlatformTrash.contains(&item));
        PlatformTrash.restore(&item, &original).unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"synthetic stub photograph");
        assert!(!data.exists(), "{}", data.display());
        assert!(!stub.exists(), "{} left behind", stub.display());
        assert!(!PlatformTrash.contains(&item));
        eprintln!("round trip on {file_system} via {}: PASS", data.display());
    }

    /// WQA7 4b: Undo of a Trash left the `$I` stubs in the Recycle Bin.
    #[test]
    #[ignore = "Uses only a fresh synthetic file in the system Bin"]
    fn platform_trash_restore_leaves_no_info_stub_in_the_bin() {
        round_trip_in(&std::env::temp_dir());
    }

    /// TRS1 (WQA7b): Trash on an exFAT volume failed its identity check.
    /// Set `RAWTONIC_TEST_TRASH_DIRS` to writable folders on disposable
    /// volumes with a Recycle Bin, separated by `;` (e.g. an NTFS and an
    /// exFAT VHDX).
    #[test]
    #[ignore = "Requires disposable volumes named in RAWTONIC_TEST_TRASH_DIRS"]
    fn platform_trash_round_trips_on_each_named_volume() {
        let directories = std::env::var("RAWTONIC_TEST_TRASH_DIRS")
            .expect("Set RAWTONIC_TEST_TRASH_DIRS to folders on disposable volumes, `;`-separated");
        for directory in directories.split(';').filter(|d| !d.is_empty()) {
            round_trip_in(Path::new(directory));
        }
    }

    #[test]
    #[ignore = "Uses only fresh synthetic files in the system Bin to verify an MTA caller"]
    fn platform_trash_works_from_an_mta_caller() {
        use windows::Win32::System::Com::COINIT_MULTITHREADED;
        std::thread::spawn(|| {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
            }
            let temp = tempfile::tempdir().unwrap();
            let original = temp.path().join("synthetic-mta-probe.txt");
            fs::write(&original, b"synthetic mta photograph").unwrap();
            let item = PlatformTrash.trash(&original).unwrap();
            assert!(PlatformTrash.contains(&item));
            PlatformTrash.restore(&item, &original).unwrap();
            assert_eq!(fs::read(&original).unwrap(), b"synthetic mta photograph");
            unsafe {
                CoUninitialize();
            }
        })
        .join()
        .unwrap();
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
    fn plain_and_verbatim_destinations_keep_the_same_receipt_and_collision_checks() {
        let (_temp, receipt) = staged_fixture();
        let plain = plain_drive_path(&receipt.original).unwrap();
        let text = serde_json::to_string(&receipt).unwrap();
        fs::write(&plain, b"unrelated destination").unwrap();
        assert!(PlatformTrash.restore(&text, &plain).is_err());
        assert_eq!(fs::read(&plain).unwrap(), b"unrelated destination");
        assert_eq!(
            fs::read(receipt.stage().unwrap().join("payload")).unwrap(),
            b"synthetic photograph"
        );
        fs::remove_file(&plain).unwrap(); // Only this fixture's synthetic collision.
        PlatformTrash.restore(&text, &plain).unwrap();
        PlatformTrash.restore(&text, &receipt.original).unwrap();
        assert_eq!(fs::read(&plain).unwrap(), b"synthetic photograph");
        let other = plain.with_file_name("another-destination.txt");
        assert!(PlatformTrash.restore(&text, &other).is_err());
        assert!(!other.exists());
    }

    /// The app records the verbatim spelling (`\\?\C:\…`) in its receipts
    /// and the journal may hand back either spelling; both name one
    /// destination, while a literal name with a trailing dot never stands
    /// for its plain sibling.
    #[test]
    fn receipt_spellings_name_one_destination_but_never_a_literal_sibling() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        #[allow(clippy::disallowed_methods)]
        let verbatim_root = fs::canonicalize(temp.path()).unwrap();
        let plain_root = plain_drive_path(&verbatim_root).unwrap();
        assert_ne!(verbatim_root, plain_root);

        // A verbatim receipt restored through the plain spelling.
        let (temp, receipt) = staged_fixture_at(temp, &verbatim_root, "photo.txt");
        let text = serde_json::to_string(&receipt).unwrap();
        PlatformTrash
            .restore(&text, &plain_root.join("photo.txt"))
            .unwrap();
        assert_eq!(
            fs::read(plain_root.join("photo.txt")).unwrap(),
            b"synthetic photograph"
        );

        // A plain receipt restored through the verbatim spelling.
        let (temp, receipt) = staged_fixture_at(temp, &plain_root, "second.txt");
        let text = serde_json::to_string(&receipt).unwrap();
        PlatformTrash
            .restore(&text, &verbatim_root.join("second.txt"))
            .unwrap();
        assert_eq!(
            fs::read(plain_root.join("second.txt")).unwrap(),
            b"synthetic photograph"
        );

        // `literal.txt.` exists only in the verbatim form; its plain spelling
        // names the sibling `literal.txt`, which must stay untouched.
        let (_temp, receipt) = staged_fixture_at(temp, &verbatim_root, "literal.txt.");
        let text = serde_json::to_string(&receipt).unwrap();
        let sibling = verbatim_root.join("literal.txt");
        assert!(PlatformTrash.restore(&text, &sibling).is_err());
        assert!(
            PlatformTrash
                .restore(&text, &plain_root.join("literal.txt"))
                .is_err()
        );
        assert!(!sibling.exists());
        PlatformTrash.restore(&text, &receipt.original).unwrap();
        assert_eq!(
            fs::read(&receipt.original).unwrap(),
            b"synthetic photograph"
        );
        assert!(!sibling.exists());
    }

    #[test]
    fn restore_is_idempotent_after_final_rename_before_journal_commit() {
        let _apartment = Apartment::new().unwrap();
        let (_temp, receipt) = staged_fixture();
        rawtonic_fs::rename_no_clobber(
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
        // A junction needs no privilege; a directory symbolic link does.
        let junction = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&stage_path)
            .arg(&moved)
            .output()
            .unwrap();
        assert!(junction.status.success(), "mklink /J failed: {junction:?}");
        assert!(PlatformTrash.restore(&text, &receipt.original).is_err());
        assert_eq!(
            fs::read(moved.join("payload")).unwrap(),
            b"synthetic photograph"
        );
        assert!(!receipt.original.exists());
        fs::remove_dir(&stage_path).unwrap(); // The junction only, not its target.
        if !link_or_skip(std::os::windows::fs::symlink_dir(&moved, &stage_path)) {
            return;
        }
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
            let root = temp.path().canonical().unwrap();
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
        let (_, plain) = verified_item(&source.canonical().unwrap()).unwrap();
        assert_eq!(plain, source);
        assert_eq!(fs::read(&source).unwrap(), b"synthetic");
    }

    #[test]
    fn shell_entry_refuses_literal_names_that_resolve_to_another_entry() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        // Verbatim: only that form can hold names ending in a dot or space.
        #[allow(clippy::disallowed_methods)]
        let root = fs::canonicalize(temp.path()).unwrap();
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
        // Verbatim: only that form can hold names ending in a dot or space.
        #[allow(clippy::disallowed_methods)]
        let root = fs::canonicalize(temp.path()).unwrap();
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
        if !link_or_skip(std::os::windows::fs::symlink_file(&source, &alias)) {
            return;
        }
        assert!(verified_item(&source).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"synthetic");
        assert!(fs::symlink_metadata(&alias).unwrap().is_symlink());
    }
}

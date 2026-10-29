// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Shell uses ordinary paths; the library retains verbatim native paths.
//! Verify the entry the Shell parsed before allowing a recycle operation.
use super::PlatformTrash;
use anyhow::{Context, Result, ensure};
use rawtonic_fileops::SystemTrash;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::OpenOptions,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf, Prefix},
};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            GetFileInformationByHandle,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            FOF_NO_CONNECTED_ELEMENTS, FOF_NO_UI, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE,
            FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
            SIGDN_FILESYSPATH,
        },
    },
    core::HSTRING,
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

impl SystemTrash for PlatformTrash {
    fn trash(&self, path: &Path) -> Result<String> {
        let _apartment = Apartment::new()?;
        let (item, plain) = verified_item(path)?;
        let before: HashSet<_> = trash::os_limited::list()?
            .into_iter()
            .map(|i| i.id)
            .collect();
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
        let receipt = trash::os_limited::list()?
            .into_iter()
            .find(|i| {
                !before.contains(&i.id)
                    && plain_drive_path(&i.original_path()).is_ok_and(|p| p == plain)
            })
            .context("The recycled item's receipt was not found")?;
        Ok(receipt.id.to_string_lossy().into_owned())
    }

    fn restore(&self, _item: &str, _original: &Path) -> Result<()> {
        anyhow::bail!(
            "Windows Recycle Bin restore is unavailable until no-clobber recovery is implemented; the item remains recoverable in the Recycle Bin"
        )
    }

    fn contains(&self, item: &str) -> bool {
        trash::os_limited::list()
            .map(|items| items.iter().any(|i| i.id.to_string_lossy() == item))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

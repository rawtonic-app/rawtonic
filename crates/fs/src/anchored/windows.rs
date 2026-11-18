// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NtOpenFile,
};
use windows_sys::Win32::Foundation::{OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo, SYNCHRONIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

pub(super) fn open_parent(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    checked(file, true)
}

pub(super) fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    let file = open_at(parent, name, FILE_GENERIC_READ, FILE_DIRECTORY_FILE).map_err(|error| {
        if error.kind() == io::ErrorKind::NotADirectory {
            return io::Error::new(io::ErrorKind::InvalidInput, error);
        }
        error
    })?;
    checked(file, true)
}

pub(super) fn open_file(parent: &File, name: &OsStr, delete: bool) -> io::Result<File> {
    let access = if delete {
        DELETE | FILE_READ_ATTRIBUTES
    } else {
        FILE_GENERIC_READ
    };
    checked(
        open_at(parent, name, access, FILE_NON_DIRECTORY_FILE)?,
        false,
    )
}

fn checked(file: File, directory: bool) -> io::Result<File> {
    let meta = file.metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !(if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        })
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

fn open_at(parent: &File, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
    let mut name: Vec<u16> = name.encode_wide().collect();
    // A colon would select an NTFS stream rather than a plain directory entry.
    if name.iter().any(|&c| c == 0 || c == u16::from(b':')) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let length =
        u16::try_from(name.len() * size_of::<u16>()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attrs = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &mut object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut file = null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: the attributes, name buffer, parent handle and outputs live through
    // the synchronous call. FILE_OPEN_REPARSE_POINT opens the leaf itself.
    let result = unsafe {
        NtOpenFile(
            &mut file,
            access | SYNCHRONIZE,
            &attrs,
            &mut status,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
    };
    if result < 0 {
        // SAFETY: this converts the returned NTSTATUS and accesses no pointers.
        let code = unsafe { RtlNtStatusToDosError(result) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: successful NtOpenFile returns a new owned handle.
    Ok(unsafe { File::from_raw_handle(file) })
}

pub(super) fn unlink(_parent: &File, _name: &OsStr, file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the handle has DELETE access and info has the layout and length
    // required by FileDispositionInfo.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

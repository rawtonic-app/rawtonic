// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Microsoft Visual C++ runtime ONNX Runtime needs on Windows (gap G76).
//!
//! Microsoft's ONNX Runtime and WebGPU plugin DLLs link the C++ standard
//! library dynamically (`MSVCP140.dll`, `MSVCP140_1.dll`,
//! `MSVCP140_ATOMIC_WAIT.dll`, `VCRUNTIME140*.dll`); the app itself links
//! the VC runtime statically. A runtime older than the toolset that built a
//! DLL is not supported by Microsoft, and since Visual Studio 2022 17.10
//! (14.40) it crashes outright: `std::mutex` became constexpr-constructible
//! and an older `MSVCP140.dll` dereferences the new layout (access violation
//! in `MSVCP140.dll`, seen with Windows' own 14.24 copy).
//!
//! The Windows builds therefore ship the current runtime app-local, beside
//! `onnxruntime.dll` in `resources\` (Microsoft's app-local deployment;
//! `src-tauri/build.rs`). `prepare` loads those copies by full path before
//! ONNX Runtime: Windows resolves an import by module name and takes a
//! module of that name that is already loaded, so ONNX Runtime and its
//! plugin bind to the bundled runtime whatever search order loads them.
//! Then it checks the `MSVCP140.dll`/`VCRUNTIME140.dll` actually in the
//! process against the linker version of the ONNX Runtime library; a
//! runtime that is too old (another module loaded Windows' copy first, or a
//! build without the bundled copies) is a clear error instead of a crash.

/// The VC++ runtime libraries the bundled ONNX Runtime and its plugins
/// import, in load order (each only after what it imports itself).
/// `vcruntime140_1.dll` exists for x64 only.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const LIBRARIES: [&str; 5] = [
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "msvcp140.dll",
    "msvcp140_1.dll",
    "msvcp140_atomic_wait.dll",
];

/// The libraries whose version is checked: the C++ library and the runtime
/// under it.
#[cfg_attr(not(windows), allow(dead_code))]
const CHECKED: [&str; 2] = ["msvcp140.dll", "vcruntime140.dll"];

/// The oldest runtime for binaries built with Visual Studio 2022 17.10 or
/// later, used when a library's linker version cannot be read.
#[cfg_attr(not(windows), allow(dead_code))]
const STL_MUTEX_FLOOR: (u16, u16) = (14, 40);

/// A file version (`major.minor.build.revision`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FileVersion(pub u16, pub u16, pub u16, pub u16);

impl std::fmt::Display for FileVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0, self.1, self.2, self.3)
    }
}

impl FileVersion {
    /// At least `major.minor`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn at_least(self, (major, minor): (u16, u16)) -> bool {
        (self.0, self.1) >= (major, minor)
    }
}

/// The linker version (`major.minor`) in a PE image's optional header, from
/// the image's first bytes; `None` for anything that is not a PE image.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn linker_version(head: &[u8]) -> Option<(u16, u16)> {
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(head.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    // Signature (4) + COFF file header (20), then the optional header:
    // magic (2), MajorLinkerVersion (1), MinorLinkerVersion (1).
    let optional = pe + 24;
    let magic = u16::from_le_bytes(head.get(optional..optional + 2)?.try_into().ok()?);
    if magic != 0x10b && magic != 0x20b {
        return None;
    }
    Some((
        u16::from(*head.get(optional + 2)?),
        u16::from(*head.get(optional + 3)?),
    ))
}

/// The runtime version `library` needs: its linker version, but never less
/// than the `std::mutex` floor (a linker that reports less is not one that
/// can be trusted to say what the STL needs).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn required_version(linker: Option<(u16, u16)>) -> (u16, u16) {
    linker.map_or(STL_MUTEX_FLOOR, |l| l.max(STL_MUTEX_FLOOR))
}

#[cfg(windows)]
pub(crate) use windows::prepare;

#[cfg(windows)]
mod windows {
    use super::{CHECKED, FileVersion, LIBRARIES, linker_version, required_version};
    use std::ffi::OsString;
    use std::io::Read;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleW, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
        LoadLibraryW,
    };

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn loaded(name: &str) -> Option<HMODULE> {
        let name = wide(name.as_ref());
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        (!module.is_null()).then_some(module)
    }

    fn module_path(module: HMODULE) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 1024];
        loop {
            let n = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) }
                as usize;
            if n == 0 {
                return None;
            }
            if n < buffer.len() {
                return Some(PathBuf::from(OsString::from_wide(&buffer[..n])));
            }
            buffer.resize(buffer.len() * 2, 0);
        }
    }

    fn file_version(path: &Path) -> Option<FileVersion> {
        let path = wide(path.as_os_str());
        let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut()) };
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
            return None;
        }
        let root = wide("\\".as_ref());
        let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) } == 0
            || info.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = unsafe { &*(info as *const VS_FIXEDFILEINFO) };
        Some(FileVersion(
            (info.dwFileVersionMS >> 16) as u16,
            info.dwFileVersionMS as u16,
            (info.dwFileVersionLS >> 16) as u16,
            info.dwFileVersionLS as u16,
        ))
    }

    fn library_linker_version(library: &Path) -> Option<(u16, u16)> {
        let mut head = vec![0u8; 4096];
        let mut file = std::fs::File::open(library).ok()?;
        let n = file.read(&mut head).ok()?;
        linker_version(&head[..n])
    }

    /// Loads the VC++ runtime bundled beside `library` (ONNX Runtime), then
    /// checks the runtime in the process is not older than `library` needs.
    /// Returns what is in use, for the log; an error says why ONNX Runtime
    /// must not be loaded. Modules are never unloaded.
    pub(crate) fn prepare(library: &Path) -> Result<String, String> {
        let dir = library.parent().filter(|d| !d.as_os_str().is_empty());
        if let Some(dir) = dir {
            for name in LIBRARIES {
                let bundled = dir.join(name);
                // A module of that name already loaded wins anyway; a second
                // copy would only confuse which one a later import gets.
                if !bundled.is_file() || loaded(name).is_some() {
                    continue;
                }
                let path = wide(bundled.as_os_str());
                let module = unsafe {
                    LoadLibraryExW(
                        path.as_ptr(),
                        std::ptr::null_mut(),
                        LOAD_WITH_ALTERED_SEARCH_PATH,
                    )
                };
                if module.is_null() {
                    return Err(format!(
                        "the Visual C++ runtime {} could not be loaded: {}",
                        bundled.display(),
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
        let required = required_version(library_linker_version(library));
        let mut in_use = Vec::new();
        for name in CHECKED {
            // What ONNX Runtime would get by the system's search, when the
            // build bundles none: loading it changes nothing it would not.
            let module = match loaded(name) {
                Some(module) => module,
                None => {
                    let w = wide(name.as_ref());
                    let module = unsafe { LoadLibraryW(w.as_ptr()) };
                    if module.is_null() {
                        // Not installed at all: ONNX Runtime's own load fails
                        // with the system's message.
                        continue;
                    }
                    module
                }
            };
            let Some(path) = module_path(module) else {
                continue;
            };
            let Some(version) = file_version(&path) else {
                in_use.push(format!("{} (version unknown)", path.display()));
                continue;
            };
            if !version.at_least(required) {
                return Err(format!(
                    "the Visual C++ runtime in use, {} {version}, is older than the {}.{} {} was built with; \
                     install the current Microsoft Visual C++ Redistributable or use a Rawtonic build that bundles it",
                    path.display(),
                    required.0,
                    required.1,
                    library.display()
                ));
            }
            in_use.push(format!("{} {version}", path.display()));
        }
        Ok(in_use.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PE header: MZ, e_lfanew = 0x40, "PE\0\0", COFF header,
    /// optional header magic and linker version.
    fn pe_head(magic: u16, major: u8, minor: u8) -> Vec<u8> {
        let mut head = vec![0u8; 0x40 + 24 + 4];
        head[..2].copy_from_slice(b"MZ");
        head[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        head[0x40..0x44].copy_from_slice(b"PE\0\0");
        let optional = 0x40 + 24;
        head[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
        head[optional + 2] = major;
        head[optional + 3] = minor;
        head
    }

    #[test]
    fn reads_the_linker_version_of_pe32_and_pe32_plus() {
        assert_eq!(linker_version(&pe_head(0x20b, 14, 44)), Some((14, 44)));
        assert_eq!(linker_version(&pe_head(0x10b, 14, 29)), Some((14, 29)));
    }

    #[test]
    fn anything_else_has_no_linker_version() {
        assert_eq!(linker_version(b""), None);
        assert_eq!(linker_version(b"\x7fELF"), None);
        let mut bad_sig = pe_head(0x20b, 14, 44);
        bad_sig[0x40] = b'X';
        assert_eq!(linker_version(&bad_sig), None);
        assert_eq!(linker_version(&pe_head(0x107, 14, 44)), None);
        let mut far = pe_head(0x20b, 14, 44);
        far[0x3c..0x40].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert_eq!(linker_version(&far), None);
    }

    #[test]
    fn needs_the_linker_version_but_never_less_than_the_mutex_floor() {
        assert_eq!(required_version(Some((14, 44))), (14, 44));
        assert_eq!(required_version(Some((14, 29))), (14, 40));
        assert_eq!(required_version(None), (14, 40));
    }

    #[test]
    fn the_crashing_system_runtime_is_too_old() {
        // Windows' own MSVCP140.dll on the test laptop (gap G76) against
        // ONNX Runtime 1.30.0's linker version.
        assert!(!FileVersion(14, 24, 28127, 4).at_least((14, 44)));
        assert!(FileVersion(14, 44, 35211, 0).at_least((14, 44)));
        assert!(FileVersion(14, 50, 0, 0).at_least((14, 44)));
        assert!(!FileVersion(14, 43, 65535, 0).at_least((14, 44)));
    }
}

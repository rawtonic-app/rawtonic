//! Open With / Edit With on Windows.
//!
//! The programs offered for a file are the shell's recommended association
//! handlers for its extension (`SHAssocEnumHandlers`, the list of
//! Explorer's "Open with" menu); a program is known by the handler's
//! `GetName` (its program file, or a packaged app's name). Opening finds
//! that program's ProgID among the type's `OpenWithProgids` and lets the
//! shell start it with each file (`ShellExecuteEx` with the class), which
//! runs the program's registered command for packaged and desktop programs
//! alike. (`IAssocHandler::Invoke` showed Windows' "Choose an app" dialog
//! instead, for Paint, Photos and the old Photo Viewer on Windows 11.) A
//! program file the user picked with "Choose program…" that is no
//! registered program for the type is started directly with the files as
//! arguments (`CreateProcess`).
//!
//! Shell objects live on a fresh single-threaded apartment thread, never a
//! reused Tauri pool thread (see `file_operations/windows_trash.rs`).
use super::{
    AppChoice, Launched,
    handlers::{self, Handler, ProgId},
};
use base64::Engine;
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, S_OK},
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP,
        },
        Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, GetShortPathNameW, VerQueryValueW,
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Registry::{
                HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumValueW,
                RegOpenKeyExW,
            },
        },
        UI::{
            Shell::{
                ASSOC_FILTER, ASSOC_FILTER_RECOMMENDED, ASSOCF_NOTRUNCATE, ASSOCSTR,
                ASSOCSTR_EXECUTABLE, ASSOCSTR_FRIENDLYAPPNAME, AssocQueryStringW, ExtractIconExW,
                IAssocHandler, SEE_MASK_CLASSNAME, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
                SHAssocEnumHandlers, SHELLEXECUTEINFOW, SHLoadIndirectString, ShellExecuteExW,
            },
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, SW_SHOWNORMAL},
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, w},
};

pub const SUPPORTED: bool = true;
/// Each handler is listed once by its id; two programs may share a name
/// (`handlers::choices` tells them apart).
pub const ONE_PER_NAME: bool = false;
/// Icons larger than this are not sent to the menu.
const MAX_ICON_BYTES: u64 = 256 * 1024;
/// The longest path a program that is not long-path aware can open
/// (`MAX_PATH` with its terminating NUL).
const MAX_PATH_CHARS: usize = 259;

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, String> {
        // SAFETY: plain COM initialisation of this (fresh) thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| e.to_string())?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: pairs the successful CoInitializeEx of `new`.
        unsafe { CoUninitialize() };
    }
}

/// Runs `job` on a fresh thread in its own single-threaded apartment.
fn shell_thread<T: Send>(job: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _apartment = Apartment::new()?;
                job()
            })
            .join()
            .map_err(|_| "The Windows shell worker stopped.".to_string())?
    })
}

/// The text of a shell string, freed.
fn take(text: PWSTR) -> String {
    // SAFETY: a NUL-terminated string the shell allocated with the COM
    // allocator, read once and freed once.
    unsafe {
        let value = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0.cast()));
        value
    }
}

fn wide(path: &Path) -> Result<Vec<u16>, String> {
    rawtonic_fs::wide_path(path).map_err(|e| e.to_string())
}

/// `.tif` for `C:\…\A_edit.TIF`.
fn extension_of(file: &Path) -> Result<String, String> {
    file.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .ok_or_else(|| format!("{} has no file type.", file.display()))
}

/// The association handlers for `extension`, with each one's icon
/// location (`path`, index).
fn enumerate(
    extension: &str,
    filter: ASSOC_FILTER,
) -> Result<Vec<(Handler, IAssocHandler)>, String> {
    // SAFETY: a valid extension string; the enumerator is released on drop.
    let list = unsafe { SHAssocEnumHandlers(&HSTRING::from(extension), filter) }
        .map_err(|e| format!("The programs for {extension} cannot be listed: {e}"))?;
    let mut found = Vec::new();
    loop {
        let mut one = [None];
        let mut fetched = 0u32;
        // SAFETY: room for one handler and a valid count pointer.
        if unsafe { list.Next(&mut one, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        let Some(handler) = one[0].take() else { break };
        // SAFETY: plain getters on a live handler.
        let (id, name, recommended) = unsafe {
            let id = handler.GetName().map(take).unwrap_or_default();
            let name = handler.GetUIName().map(take).unwrap_or_else(|_| id.clone());
            (id, name, handler.IsRecommended() == S_OK)
        };
        found.push((
            Handler {
                id,
                name,
                recommended,
            },
            handler,
        ));
    }
    Ok(found)
}

/// `AssocQueryString` for a file type (`.tif`: its default program) or a
/// ProgID.
fn assoc_string(extension: &str, what: ASSOCSTR) -> Option<String> {
    let mut buffer = vec![0u16; 2048];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` characters.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_NOTRUNCATE,
            what,
            &HSTRING::from(extension),
            None,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    if result.is_err() || len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..len as usize - 1]))
}

fn data_url(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

/// The icon of a handler as a small PNG data URL.
fn handler_icon(handler: &IAssocHandler) -> Option<String> {
    let mut location = PWSTR::null();
    let mut index = 0;
    // SAFETY: valid out pointers; the string is freed by `take`.
    unsafe { handler.GetIconLocation(&mut location, &mut index) }.ok()?;
    let location = take(location);
    if location.starts_with('@') {
        // A packaged app's logo: `@{Package?ms-resource://…/Logo.png}`
        // names a PNG file in its package.
        let mut buffer = vec![0u16; 1024];
        // SAFETY: an indirect string and a buffer for the answer.
        unsafe { SHLoadIndirectString(&HSTRING::from(location.as_str()), &mut buffer, None) }
            .ok()?;
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let file = PathBuf::from(OsString::from_wide(&buffer[..end]));
        let is_png = file
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= MAX_ICON_BYTES);
        return (is_png && small)
            .then(|| std::fs::read(&file).ok())
            .flatten()
            .map(|png| data_url(&png));
    }
    file_icon(Path::new(&location), index)
}

/// Icon `index` of a program file (`.exe`, `.dll`) as a PNG data URL.
fn file_icon(file: &Path, index: i32) -> Option<String> {
    let path = wide(file).ok()?;
    let mut icon = HICON::default();
    // SAFETY: a NUL-terminated path and room for one large icon.
    let count = unsafe { ExtractIconExW(PCWSTR(path.as_ptr()), index, Some(&mut icon), None, 1) };
    if count == 0 || icon.is_invalid() {
        return None;
    }
    let png = icon_png(icon);
    // SAFETY: the icon ExtractIconExW made, destroyed once.
    let _ = unsafe { DestroyIcon(icon) };
    png.map(|png| data_url(&png))
}

/// The pixels of `bitmap` as top-down BGRA rows.
fn bitmap_pixels(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = BITMAP::default();
    // SAFETY: `info` is a BITMAP of the size given.
    let got = unsafe {
        GetObjectW(
            bitmap,
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut info as *mut BITMAP).cast()),
        )
    };
    let (width, height) = (info.bmWidth, info.bmHeight);
    if got == 0 || width <= 0 || height <= 0 || width > 256 || height > 256 {
        return None;
    }
    let mut header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: a memory DC, released below; `pixels` holds `height` rows of
    // 32-bit pixels as `header` describes.
    let rows = unsafe {
        let dc = CreateCompatibleDC(None);
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut header,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        rows
    };
    (rows == height).then_some((width as u32, height as u32, pixels))
}

/// An icon as PNG bytes; an icon without alpha takes its mask.
fn icon_png(icon: HICON) -> Option<Vec<u8>> {
    let mut info = ICONINFO::default();
    // SAFETY: `info` receives two bitmaps, deleted below.
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let color = bitmap_pixels(info.hbmColor);
    let mask = bitmap_pixels(info.hbmMask);
    // SAFETY: the bitmaps GetIconInfo made, each deleted once.
    unsafe {
        let _ = DeleteObject(info.hbmColor);
        let _ = DeleteObject(info.hbmMask);
    }
    let (width, height, mut pixels) = color?;
    let no_alpha = pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        pixel.swap(0, 2);
        if no_alpha {
            // The mask's set pixels are transparent.
            let opaque = mask
                .as_ref()
                .filter(|(w, h, _)| (*w, *h) == (width, height))
                .is_none_or(|(_, _, m)| m[i * 4] == 0);
            pixel[3] = if opaque { 255 } else { 0 };
        }
    }
    let image = image::RgbaImage::from_raw(width, height, pixels)?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// A program file's description from its version resource
/// ("Adobe Photoshop 2026"), if it has one.
fn description(program: &Path) -> Option<String> {
    let path = wide(program).ok()?;
    // SAFETY: a NUL-terminated path; `data` is as large as asked for, and
    // the values VerQueryValueW points to lie inside it.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), 0, size, data.as_mut_ptr().cast()).ok()?;
        let mut value = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            w!("\\VarFileInfo\\Translation"),
            &mut value,
            &mut len,
        )
        .as_bool()
            || len < 4
        {
            return None;
        }
        let pair = value as *const u16;
        let (language, codepage) = (*pair, *pair.add(1));
        let key = HSTRING::from(format!(
            "\\StringFileInfo\\{language:04x}{codepage:04x}\\FileDescription"
        ));
        if !VerQueryValueW(data.as_ptr().cast(), &key, &mut value, &mut len).as_bool() || len == 0 {
            return None;
        }
        let text = std::slice::from_raw_parts(value as *const u16, len as usize);
        let text = String::from_utf16_lossy(text);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}

/// The programs that open every one of `files` and the default program of
/// the first.
pub fn apps_for(files: &[&Path]) -> (Vec<AppChoice>, Option<String>) {
    let extensions: Vec<String> = files.iter().filter_map(|f| extension_of(f).ok()).collect();
    let listed = shell_thread(|| {
        let own = std::env::current_exe().ok();
        let mut per_type = Vec::new();
        let mut first = Vec::new();
        for (i, extension) in extensions.iter().enumerate() {
            let found = enumerate(extension, ASSOC_FILTER_RECOMMENDED)?;
            let handlers: Vec<Handler> = found.iter().map(|(h, _)| h.clone()).collect();
            per_type.push(handlers::offered(handlers, own.as_deref()));
            if i == 0 {
                first = found;
            }
        }
        let offered = handlers::common(per_type);
        let default = extensions.first().and_then(|extension| {
            handlers::default_id(
                &offered,
                assoc_string(extension, ASSOCSTR_EXECUTABLE).as_deref(),
                assoc_string(extension, ASSOCSTR_FRIENDLYAPPNAME).as_deref(),
            )
        });
        let icon = |handler: &Handler| {
            first
                .iter()
                .find(|(h, _)| h.id == handler.id)
                .and_then(|(_, shell)| handler_icon(shell))
        };
        Ok((handlers::choices(&offered, &icon), default))
    });
    listed.unwrap_or_else(|e| {
        log::warn!("Open With: {e}");
        (Vec::new(), None)
    })
}

/// A program the user picked with "Choose program…", as a menu entry,
/// while its file exists.
pub fn describe(program: &str) -> Option<AppChoice> {
    if !handlers::is_program_file(program) || !Path::new(program).is_file() {
        return None;
    }
    let path = Path::new(program);
    Some(AppChoice {
        path: program.to_string(),
        name: description(path).unwrap_or_else(|| handlers::program_file_name(program)),
        is_default: false,
        is_recent: false,
        icon: file_icon(path, 0),
    })
}

/// Any program id can be tried: `open` finds out whether it exists.
pub fn check(program: &str) -> Result<(), String> {
    if program.is_empty() || program.contains('\0') {
        Err(format!("{program:?} is not a program."))
    } else {
        Ok(())
    }
}

/// The spelling of `path` to hand to another program: absolute with `\`
/// only (the shell refuses `C:\Photos/A.tif`, which the frontend's joins
/// can give, with E_INVALIDARG). Most programs open no path of 260
/// characters or more; such a path is handed in its short (8.3) form when
/// the volume has one, and as it is otherwise.
fn handoff_path(path: &Path) -> PathBuf {
    let normal = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = normal.as_path();
    if path.as_os_str().encode_wide().count() <= MAX_PATH_CHARS {
        return path.to_path_buf();
    }
    let Ok(long) = wide(path) else {
        return path.to_path_buf();
    };
    // SAFETY: a NUL-terminated path; the second call gets a buffer of the
    // size the first one asked for.
    let short = unsafe {
        let size = GetShortPathNameW(PCWSTR(long.as_ptr()), None);
        if size == 0 {
            return path.to_path_buf();
        }
        let mut buffer = vec![0u16; size as usize];
        let len = GetShortPathNameW(PCWSTR(long.as_ptr()), Some(&mut buffer)) as usize;
        if len == 0 || len >= buffer.len() {
            return path.to_path_buf();
        }
        rawtonic_fs::simplified(&PathBuf::from(OsString::from_wide(&buffer[..len])))
    };
    if short.as_os_str().encode_wide().count() <= MAX_PATH_CHARS {
        short
    } else {
        path.to_path_buf()
    }
}

/// The values of a registry key (`OpenWithProgids` lists ProgIDs as value
/// names).
fn value_names(root: HKEY, key: &str) -> Vec<String> {
    let mut opened = HKEY::default();
    // SAFETY: a valid key name and out handle, closed below.
    if unsafe { RegOpenKeyExW(root, &HSTRING::from(key), 0, KEY_READ, &mut opened) }
        != ERROR_SUCCESS
    {
        return Vec::new();
    }
    let mut names = Vec::new();
    for index in 0.. {
        let mut name = vec![0u16; 512];
        let mut len = name.len() as u32;
        // SAFETY: `name` holds `len` characters; no type or data asked for.
        let result = unsafe {
            RegEnumValueW(
                opened,
                index,
                PWSTR(name.as_mut_ptr()),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if result != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    // SAFETY: the key opened above, closed once.
    let _ = unsafe { RegCloseKey(opened) };
    names
}

/// The ProgIDs registered to open `extension` (the machine's and the
/// user's classes, and the user's Explorer list), each described.
fn progids(extension: &str) -> Vec<ProgId> {
    let mut classes = value_names(HKEY_CLASSES_ROOT, &format!(r"{extension}\OpenWithProgids"));
    classes.extend(value_names(
        HKEY_CURRENT_USER,
        &format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{extension}\OpenWithProgids"),
    ));
    let mut seen = std::collections::HashSet::new();
    classes
        .into_iter()
        .filter(|class| !class.is_empty() && seen.insert(class.to_lowercase()))
        .map(|class| ProgId {
            executable: assoc_string(&class, ASSOCSTR_EXECUTABLE),
            friendly: assoc_string(&class, ASSOCSTR_FRIENDLYAPPNAME),
            class,
        })
        .collect()
}

/// Lets the shell open `file` with the registered program `class` (a
/// ProgID).
fn open_as(class: &str, file: &Path) -> Result<(), String> {
    let file_text = HSTRING::from(file);
    let class_text = HSTRING::from(class);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // No error dialog (the caller reports it); wait for the start on
        // this short-lived thread.
        fMask: SEE_MASK_CLASSNAME | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file_text.as_ptr()),
        lpClass: PCWSTR(class_text.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to live for the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| format!("{}: {e}", file.display()))
}

/// Starts the program file `program` with `files` as its arguments, in as
/// many starts as the command-line limit needs.
fn start_program(program: &str, files: &[PathBuf]) -> Result<(), String> {
    let files: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    for batch in handlers::command_batches(program, &files, handlers::MAX_COMMAND_LINE) {
        Command::new(program)
            .args(batch)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| format!("{program} could not be started: {e}"))?;
    }
    Ok(())
}

/// Whether the classes have `key` (`Applications\editor.exe\shell`).
fn has_key(key: &str) -> bool {
    let mut opened = HKEY::default();
    // SAFETY: a valid key name and out handle, closed below.
    if unsafe {
        RegOpenKeyExW(
            HKEY_CLASSES_ROOT,
            &HSTRING::from(key),
            0,
            KEY_READ,
            &mut opened,
        )
    } != ERROR_SUCCESS
    {
        return false;
    }
    // SAFETY: the key opened above, closed once.
    let _ = unsafe { RegCloseKey(opened) };
    true
}

/// Opens `files` in `program`: through the shell with the program's ProgID
/// for the first file's type, or its `Applications\` registration (each
/// file), or, for a program file without either (picked by the user), by
/// starting it with the files as arguments. `Picked` when the program is
/// not among the ones offered for the type.
pub fn open(program: &str, files: &[PathBuf]) -> Result<Launched, String> {
    let first = files.first().ok_or("No files to open.")?;
    let extension = extension_of(first)?;
    let handoff: Vec<PathBuf> = files.iter().map(|f| handoff_path(f)).collect();
    let program_file = handlers::is_program_file(program) && Path::new(program).is_file();
    shell_thread(|| {
        let offered = enumerate(&extension, ASSOC_FILTER_RECOMMENDED)?
            .iter()
            .any(|(h, _)| h.recommended && handlers::same_id(&h.id, program));
        let launched = if offered {
            Launched::Listed
        } else {
            Launched::Picked
        };
        // A program registered by its file name only (Explorer's
        // `OpenWithList`) keeps its own command under `Applications\`.
        let registered = Path::new(program)
            .file_name()
            .map(|name| format!(r"Applications\{}", name.to_string_lossy()))
            .filter(|class| program_file && has_key(&format!(r"{class}\shell")));
        if let Some(class) = handlers::progid_for(program, &progids(&extension)) {
            for file in &handoff {
                open_as(&class, file)?;
            }
            return Ok(launched);
        }
        if !program_file {
            return Err(format!("{program} is not available."));
        }
        // Such a registration can be stale: System32's `notepad.exe` has
        // one whose command Windows 11 refuses ("not a valid Win32
        // application"). Then the program file is started itself.
        if let Some(class) = registered
            && open_as(&class, &handoff[0]).is_ok()
        {
            for file in &handoff[1..] {
                open_as(&class, file)?;
            }
            return Ok(launched);
        }
        start_program(program, &handoff)?;
        Ok(launched)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell of this Windows system lists programs for a JPEG, with
    /// names and icons, and Rawtonic's test binary is not among them.
    #[test]
    #[ignore = "needs a Windows desktop with the usual image programs"]
    fn windows_lists_the_programs_for_a_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("probe.jpg");
        std::fs::write(&file, b"").unwrap();
        let (apps, default) = apps_for(&[file.as_path()]);
        for app in &apps {
            eprintln!(
                "{} | {} | icon {:?}",
                app.name,
                app.path,
                app.icon.as_ref().map(String::len)
            );
        }
        eprintln!("default {default:?}");
        assert!(!apps.is_empty());
        assert!(apps.iter().any(|a| a.icon.is_some()));
    }

    /// The processes whose command line names `file`, after waiting up to
    /// 20 s for one.
    fn started_with(file: &Path) -> Vec<sysinfo::Pid> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let wanted = file.to_string_lossy().to_lowercase();
        let mut system = System::new();
        for _ in 0..40 {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
            );
            let found: Vec<_> = system
                .processes()
                .iter()
                .filter(|(_, p)| {
                    p.cmd()
                        .iter()
                        .any(|arg| arg.to_string_lossy().to_lowercase().contains(&wanted))
                })
                .map(|(pid, _)| *pid)
                .collect();
            if !found.is_empty() {
                return found;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Vec::new()
    }

    fn stop(pids: &[sysinfo::Pid]) {
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(pids), true);
        for pid in pids {
            if let Some(process) = system.process(*pid) {
                process.kill();
            }
        }
    }

    /// Paint (a registered handler, started by the shell) and a picked
    /// program file (started directly) open a render whose path has
    /// spaces and non-ASCII names. Run in the desktop session: it starts
    /// the programs and stops them again.
    #[test]
    #[ignore = "starts Paint and Notepad on a Windows desktop"]
    fn windows_starts_a_listed_and_a_picked_program_with_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Fotos mit Leerzeichen/Größe ändern");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("Bild Ä_edit.tiff");
        // A 1×1 white TIFF.
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 255, 255]));
        image.save(&file).unwrap();
        let before = std::fs::read(&file).unwrap();

        let (apps, _) = apps_for(&[file.as_path()]);
        let paint = apps
            .iter()
            .find(|a| a.path.to_lowercase().ends_with(r"\mspaint.exe"))
            .expect("Paint is offered for TIFF");
        assert_eq!(
            open(&paint.path, std::slice::from_ref(&file)),
            Ok(Launched::Listed)
        );
        let pids = started_with(&handoff_path(&file));
        eprintln!("Paint: {pids:?}");
        assert!(
            !pids.is_empty(),
            "Paint did not start with {}",
            file.display()
        );
        stop(&pids);

        // A packaged app without a program file (Photos), by its name.
        if let Some(photos) = apps.iter().find(|a| !a.path.contains('\\')) {
            assert_eq!(
                open(&photos.path, std::slice::from_ref(&file)),
                Ok(Launched::Listed)
            );
            let pids = started_with(&handoff_path(&file));
            eprintln!("{}: {pids:?}", photos.name);
            assert!(!pids.is_empty(), "{} did not start", photos.name);
            stop(&pids);
        }

        let text = folder.join("Notiz Ü.txt");
        std::fs::write(&text, "Rawtonic").unwrap();
        let notepad = format!(
            r"{}\System32\notepad.exe",
            std::env::var("SystemRoot").unwrap()
        );
        assert_eq!(
            open(&notepad, std::slice::from_ref(&text)),
            Ok(Launched::Picked)
        );
        let pids = started_with(&handoff_path(&text));
        eprintln!("Notepad: {pids:?}");
        assert!(
            !pids.is_empty(),
            "Notepad did not start with {}",
            text.display()
        );
        stop(&pids);
        // The program that is no handler and no file is refused.
        assert!(open(r"C:\No\Such\Editor.exe", std::slice::from_ref(&file)).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }

    #[test]
    fn long_paths_are_handed_over_short_when_the_volume_has_short_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut folder = tmp.path().to_path_buf();
        while folder.as_os_str().len() < 300 {
            folder.push("Ein ziemlich langer Ordnername für Fotos");
        }
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("Größe 1_edit.tiff");
        std::fs::write(&file, b"tiff").unwrap();
        let handed = handoff_path(&file);
        // A volume without 8.3 names keeps the long path; with them the
        // short one names the same file.
        if handed != file {
            assert!(handed.as_os_str().len() <= MAX_PATH_CHARS, "{handed:?}");
            assert_eq!(std::fs::read(&handed).unwrap(), b"tiff");
        }
        let plain = tmp.path().join("Größe 2 mit Leerzeichen.tiff");
        assert_eq!(handoff_path(&plain), plain);
        // Forward slashes from the frontend's joins become backslashes.
        let mixed = PathBuf::from(format!("{}/Fotos/A.tiff", tmp.path().display()));
        assert_eq!(handoff_path(&mixed), tmp.path().join(r"Fotos\A.tiff"));
    }

    #[test]
    fn a_picked_program_is_described_by_its_version_resource() {
        let notepad = std::env::var("SystemRoot")
            .map(|root| format!(r"{root}\System32\notepad.exe"))
            .unwrap();
        let choice = describe(&notepad).expect("notepad is a program file");
        assert!(!choice.name.is_empty() && !choice.name.ends_with(".exe"));
        assert!(
            choice
                .icon
                .is_some_and(|i| i.starts_with("data:image/png;base64,"))
        );
        assert!(describe(r"C:\no\such\program.exe").is_none());
        assert!(describe("Windows-Fotoanzeige").is_none());
    }
}

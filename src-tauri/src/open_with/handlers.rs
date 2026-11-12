//! The programs Windows offers for a file type, as Open With / Edit With
//! list them: what the shell's association handlers (`SHAssocEnumHandlers`)
//! report, turned into menu entries. Plain data, so it is tested on every
//! system; `windows_shell.rs` reads the handlers.
use super::AppChoice;
use std::path::Path;

/// One association handler as the shell reports it (`IAssocHandler`).
#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    /// `GetName`: the program file of a desktop program
    /// (`C:\…\mspaint.exe`, `…\PhotoViewer.dll`), the display name of a
    /// packaged one. The program's id in Open With / Edit With.
    pub id: String,
    /// `GetUIName`: the name Explorer shows.
    pub name: String,
    /// `IsRecommended`: listed in Explorer's "Open with" menu.
    pub recommended: bool,
}

/// Same id: Windows names are case-insensitive.
pub fn same_id(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The file name of a program file id, as written (`Path` on a Unix test
/// host does not split at `\`); none for a packaged program's name.
fn file_part(id: &str) -> Option<&str> {
    let name = id.rsplit(['\\', '/']).next()?;
    (name.len() < id.len()).then_some(name)
}

fn file_name(id: &str) -> Option<String> {
    file_part(id).map(str::to_lowercase)
}

/// A copy of Rawtonic itself (this build, another install or a QA build):
/// its program file has our executable's name.
pub fn is_own(handler: &Handler, own_exe: Option<&Path>) -> bool {
    let own = own_exe.and_then(|exe| file_name(&exe.to_string_lossy()));
    own.is_some() && file_name(&handler.id) == own
}

/// The handlers a menu offers: the recommended ones, without Rawtonic,
/// each once.
pub fn offered(handlers: Vec<Handler>, own_exe: Option<&Path>) -> Vec<Handler> {
    let mut kept: Vec<Handler> = Vec::new();
    for handler in handlers {
        if handler.recommended
            && !handler.id.is_empty()
            && !is_own(&handler, own_exe)
            && !kept.iter().any(|k| same_id(&k.id, &handler.id))
        {
            kept.push(handler);
        }
    }
    kept
}

/// The handlers offered for every one of the file types (`per_type`, one
/// list per type), in the first type's order.
pub fn common(mut per_type: Vec<Vec<Handler>>) -> Vec<Handler> {
    if per_type.is_empty() {
        return Vec::new();
    }
    let first = per_type.remove(0);
    first
        .into_iter()
        .filter(|h| {
            per_type
                .iter()
                .all(|list| list.iter().any(|o| same_id(&o.id, &h.id)))
        })
        .collect()
}

/// The id of the type's default program among `handlers`. `executable` is
/// what `AssocQueryString(ASSOCSTR_EXECUTABLE)` gives (a desktop program's
/// file; none for a packaged default), `friendly` its
/// `ASSOCSTR_FRIENDLYAPPNAME`, which is a packaged program's id.
pub fn default_id(
    handlers: &[Handler],
    executable: Option<&str>,
    friendly: Option<&str>,
) -> Option<String> {
    let by = |wanted: Option<&str>, key: fn(&Handler) -> &str| {
        let wanted = wanted.filter(|w| !w.is_empty())?;
        handlers
            .iter()
            .find(|h| same_id(key(h), wanted))
            .map(|h| h.id.clone())
    };
    by(executable, |h| &h.id)
        .or_else(|| by(friendly, |h| &h.id))
        .or_else(|| by(friendly, |h| &h.name))
}

/// Menu entries for `handlers`. Two programs with one name (the Photos app
/// and the old Photo Viewer can both be "Windows Photo Viewer") get their
/// program file's name added, so the menu tells them apart.
pub fn choices(handlers: &[Handler], icon: &dyn Fn(&Handler) -> Option<String>) -> Vec<AppChoice> {
    handlers
        .iter()
        .map(|handler| {
            let shared = handlers
                .iter()
                .filter(|h| h.name.to_lowercase() == handler.name.to_lowercase())
                .count()
                > 1;
            let name = match file_part(&handler.id) {
                Some(file) if shared => {
                    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
                    format!("{} ({stem})", handler.name)
                }
                _ => handler.name.clone(),
            };
            AppChoice {
                path: handler.id.clone(),
                name,
                is_default: false,
                is_recent: false,
                icon: icon(handler),
            }
        })
        .collect()
}

/// A file type's program registration (a ProgID of its `OpenWithProgids`)
/// as `AssocQueryString` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgId {
    pub class: String,
    /// `ASSOCSTR_EXECUTABLE`; none for most packaged apps (Photos).
    pub executable: Option<String>,
    /// `ASSOCSTR_FRIENDLYAPPNAME`.
    pub friendly: Option<String>,
}

/// The ProgID that starts the handler `id`: the one whose program file is
/// that id, else (a packaged app without a program file, whose handler id
/// is its name) the one without a program file named so. The shell starts
/// a program reliably through its ProgID (`ShellExecuteEx` with the class);
/// `IAssocHandler::Invoke` showed Windows' "Choose an app" dialog instead
/// on Windows 11 (task WEW).
pub fn progid_for(id: &str, progids: &[ProgId]) -> Option<String> {
    progids
        .iter()
        .find(|p| p.executable.as_deref().is_some_and(|exe| same_id(exe, id)))
        .or_else(|| {
            progids.iter().find(|p| {
                p.executable.is_none()
                    && p.friendly.as_deref().is_some_and(|name| same_id(name, id))
            })
        })
        .map(|p| p.class.clone())
}

/// The longest command line Windows starts (`CreateProcess`: 32,767
/// characters with its terminating NUL).
pub const MAX_COMMAND_LINE: usize = 32_766;

/// The characters an argument takes on a command line: quoted, with a
/// space before it, and room for doubled backslashes before the quote.
fn argument_len(arg: &str) -> usize {
    let units = arg.encode_utf16().count();
    let trailing = arg.chars().rev().take_while(|&c| c == '\\').count();
    units + trailing + 3
}

/// The files of one start each, in order, so that no command line of
/// `program` with its files is longer than `limit`: a large selection is
/// handed over in several starts of the program. A file too long for any
/// command line on its own gets a start of its own (and fails there).
pub fn command_batches<'a>(program: &str, files: &'a [String], limit: usize) -> Vec<&'a [String]> {
    let base = argument_len(program);
    let mut batches = Vec::new();
    let mut start = 0;
    let mut len = base;
    for (i, file) in files.iter().enumerate() {
        let arg = argument_len(file);
        if i > start && len + arg > limit {
            batches.push(&files[start..i]);
            start = i;
            len = base;
        }
        len += arg;
    }
    if start < files.len() {
        batches.push(&files[start..]);
    }
    batches
}

/// A program file the user picked with "Choose program…": an absolute path
/// to an `.exe`.
pub fn is_program_file(program: &str) -> bool {
    // Written out rather than `Path::is_absolute`, which does not know
    // drive letters on a Unix test host.
    let bytes = program.as_bytes();
    let absolute = program.starts_with(r"\\")
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && matches!(bytes.get(1..3), Some(b":\\" | b":/")));
    absolute && file_name(program).is_some_and(|name| name.ends_with(".exe") && name.len() > 4)
}

/// The name of a program file for the menu when it has no description:
/// `C:\Tools\Photo Editor.exe` → `Photo Editor`.
pub fn program_file_name(program: &str) -> String {
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program);
    match name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") && !stem.is_empty() => stem.into(),
        _ => name.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(id: &str, name: &str, recommended: bool) -> Handler {
        Handler {
            id: id.into(),
            name: name.into(),
            recommended,
        }
    }

    /// What `SHAssocEnumHandlers(".tif")` reported on a Windows 11 VM
    /// (task WEW), German system language.
    fn tif() -> Vec<Handler> {
        vec![
            handler(
                r"C:\Users\me\AppData\Local\Rawtonic\rawtonic-app.exe",
                "Rawtonic",
                true,
            ),
            handler("Windows-Fotoanzeige", "Windows-Fotoanzeige", true),
            handler(
                r"C:\Program Files\WindowsApps\Microsoft.ScreenSketch_11.2608.41.0_x64__8wekyb3d8bbwe\SnippingTool\SnippingTool.exe",
                "Snipping Tool",
                true,
            ),
            handler(
                r"C:\Program Files\WindowsApps\Microsoft.Paint_11.2609.401.0_x64__8wekyb3d8bbwe\PaintApp\mspaint.exe",
                "Paint",
                true,
            ),
            handler(
                r"C:\Program Files\Windows Photo Viewer\PhotoViewer.dll",
                "Windows-Fotoanzeige",
                true,
            ),
            handler(
                r"C:\Program Files\WindowsApps\Microsoft.WindowsNotepad_11.2607.14.0_x64__8wekyb3d8bbwe\Notepad\Notepad.exe",
                "Editor",
                false,
            ),
        ]
    }

    #[test]
    fn the_recommended_handlers_are_offered_without_rawtonic() {
        // Another install of Rawtonic (the QA build) has the same file name.
        let own = Path::new(r"D:\QA\RawtonicQA\rawtonic-app.exe");
        let mut handlers = tif();
        handlers.push(handler(
            r"C:\PROGRAM FILES\WINDOWS PHOTO VIEWER\PHOTOVIEWER.DLL",
            "x",
            true,
        ));
        let ids: Vec<_> = offered(handlers, Some(own))
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(
            ids,
            [
                "Windows-Fotoanzeige",
                "Snipping Tool",
                "Paint",
                "Windows-Fotoanzeige"
            ]
        );
        // Without knowing our own file, Rawtonic is not hidden by guesswork.
        assert_eq!(offered(tif(), None).len(), 5);
    }

    #[test]
    fn programs_sharing_a_name_show_their_file() {
        let offered = offered(tif(), None);
        let names: Vec<_> = choices(&offered, &|_| None)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "Rawtonic",
                "Windows-Fotoanzeige",
                "Snipping Tool",
                "Paint",
                "Windows-Fotoanzeige (PhotoViewer)",
            ]
        );
    }

    #[test]
    fn the_default_is_found_for_desktop_and_packaged_programs() {
        let handlers = offered(tif(), None);
        // A packaged default: no executable, its friendly name is its id.
        assert_eq!(
            default_id(&handlers, None, Some("Windows-Fotoanzeige")).as_deref(),
            Some("Windows-Fotoanzeige")
        );
        // A desktop default, in another case.
        assert_eq!(
            default_id(
                &handlers,
                Some(r"c:\program files\windows photo viewer\photoviewer.dll"),
                Some("Windows-Fotoanzeige")
            )
            .as_deref(),
            Some(r"C:\Program Files\Windows Photo Viewer\PhotoViewer.dll")
        );
        // Only the friendly name of a desktop program matches its UI name.
        assert_eq!(
            default_id(&handlers, None, Some("paint")).as_deref(),
            Some(
                r"C:\Program Files\WindowsApps\Microsoft.Paint_11.2609.401.0_x64__8wekyb3d8bbwe\PaintApp\mspaint.exe"
            )
        );
        assert_eq!(default_id(&handlers, Some(""), Some("Gone")), None);
    }

    #[test]
    fn several_file_types_offer_the_programs_common_to_all() {
        let nef: Vec<_> = tif()
            .into_iter()
            .filter(|h| h.name != "Paint")
            .map(|mut h| {
                h.id = h.id.to_uppercase();
                h
            })
            .collect();
        let names: Vec<_> = common(vec![tif(), nef])
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(
            names,
            [
                "Rawtonic",
                "Windows-Fotoanzeige",
                "Snipping Tool",
                "Windows-Fotoanzeige",
                "Editor"
            ]
        );
        assert!(common(Vec::new()).is_empty());
    }

    /// `OpenWithProgids` of `.tiff` on the same VM.
    fn tiff_progids() -> Vec<ProgId> {
        let progid = |class: &str, executable: Option<&str>, friendly: &str| ProgId {
            class: class.into(),
            executable: executable.map(Into::into),
            friendly: Some(friendly.into()),
        };
        vec![
            progid(
                "AppX4mntx4h978m1v9gtzv0ewksfd6pmwsre",
                None,
                "Windows-Fotoanzeige",
            ),
            progid(
                "AppXcesbfs704v2mjbts9dkr42s9vmrhxbkj",
                Some(
                    r"C:\Program Files\WindowsApps\Microsoft.Paint_11.2609.401.0_x64__8wekyb3d8bbwe\PaintApp\mspaint.exe",
                ),
                "Paint",
            ),
            progid(
                "TIFImage.Document",
                Some(r"C:\Program Files\Windows Photo Viewer\PhotoViewer.dll"),
                "Windows-Fotoanzeige",
            ),
        ]
    }

    #[test]
    fn each_offered_handler_is_started_through_its_progid() {
        let progids = tiff_progids();
        let class = |handler: &Handler| progid_for(&handler.id, &progids);
        let offered = offered(
            tif(),
            Some(Path::new(
                r"C:\Users\me\AppData\Local\Rawtonic\rawtonic-app.exe",
            )),
        );
        let classes: Vec<_> = offered
            .iter()
            .map(|h| (h.name.as_str(), class(h)))
            .collect();
        assert_eq!(
            classes,
            [
                // The packaged Photos app by its name, the old viewer of the
                // same name by its program file.
                (
                    "Windows-Fotoanzeige",
                    Some("AppX4mntx4h978m1v9gtzv0ewksfd6pmwsre".to_string())
                ),
                ("Snipping Tool", None),
                (
                    "Paint",
                    Some("AppXcesbfs704v2mjbts9dkr42s9vmrhxbkj".to_string())
                ),
                ("Windows-Fotoanzeige", Some("TIFImage.Document".to_string())),
            ]
        );
        // In another case.
        assert_eq!(
            progid_for(
                r"c:\program files\windows photo viewer\photoviewer.dll",
                &progids
            )
            .as_deref(),
            Some("TIFImage.Document")
        );
    }

    #[test]
    fn a_large_selection_is_handed_over_in_starts_that_fit_the_command_line() {
        let program = r"C:\Program Files\Viewer\viewer.exe".to_string();
        // 200 photos with 170-character paths: 34,000 characters in one.
        let files: Vec<String> = (0..200)
            .map(|i| format!(r"D:\Fotos\{}\Größe {i:03}.tiff", "x".repeat(150)))
            .collect();
        let batches = command_batches(&program, &files, MAX_COMMAND_LINE);
        assert!(batches.len() >= 2, "{}", batches.len());
        assert_eq!(batches.concat(), files);
        for batch in &batches {
            let line: usize =
                argument_len(&program) + batch.iter().map(|f| argument_len(f)).sum::<usize>();
            assert!(line <= MAX_COMMAND_LINE, "{line}");
        }
        // A few files go in one start; nothing in none.
        assert_eq!(
            command_batches(&program, &files[..3], MAX_COMMAND_LINE).len(),
            1
        );
        assert!(command_batches(&program, &[], MAX_COMMAND_LINE).is_empty());
        // A file too long for any line still gets its own start.
        let huge = vec!["a".repeat(40_000), "b".into()];
        assert_eq!(command_batches(&program, &huge, MAX_COMMAND_LINE).len(), 2);
    }

    #[test]
    fn a_picked_program_is_an_absolute_exe() {
        assert!(is_program_file(
            r"C:\Program Files\Adobe\Photoshop 2026\Photoshop.exe"
        ));
        assert!(is_program_file(
            r"D:\Tools\Bild Bearbeiter\Größe ändern.EXE"
        ));
        assert!(is_program_file(r"\\server\share\editor.exe"));
        assert!(!is_program_file("Windows-Fotoanzeige"));
        assert!(!is_program_file(r"editor.exe"));
        assert!(!is_program_file(r"C:\Tools\editor.lnk"));
        assert!(!is_program_file(r"C:\Tools\.exe"));
        assert_eq!(
            program_file_name(r"D:\Tools\Bild Bearbeiter\Größe ändern.EXE"),
            "Größe ändern"
        );
        assert_eq!(program_file_name(r"C:\Tools\viewer"), "viewer");
    }
}

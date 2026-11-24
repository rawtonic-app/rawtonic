//! The programs a freedesktop desktop offers for a file, as Open With / Edit
//! With list them: the `.desktop` entries under `$XDG_DATA_DIRS/applications`
//! (Flatpak and Snap export theirs there), their MIME associations and raster
//! editor categories, the defaults in `mimeapps.list`, and launch commands
//! with their `Exec` field codes expanded. Plain data and file reads under
//! given directories, so it is tested on every system; `open_with.rs` binds
//! it to the environment on Linux.
//!
//! Nothing here starts a shell: `Exec` is split by the specification's own
//! quoting rules and handed to the program as separate arguments, so a file
//! name never becomes a command.
use super::AppChoice;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// The directories the desktop's data and configuration live in.
#[derive(Debug, Clone, Default)]
pub struct Dirs {
    /// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, most specific first.
    pub data: Vec<PathBuf>,
    /// `$XDG_CONFIG_HOME` then `$XDG_CONFIG_DIRS`.
    pub config: Vec<PathBuf>,
    /// `$XDG_CURRENT_DESKTOP`, lower-cased, for `<desktop>-mimeapps.list`.
    pub desktops: Vec<String>,
}

impl Dirs {
    /// The directories of the running session, following the XDG base
    /// directory defaults. Flatpak's and Snap's export directories are
    /// added when a minimal `XDG_DATA_DIRS` leaves them out.
    // Only `linux.rs` reads the environment; the tests of other systems
    // pass directories.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let list = |value: String| -> Vec<PathBuf> {
            value
                .split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        };
        let mut data = Vec::new();
        data.extend(
            var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(".local/share"))),
        );
        data.extend(list(
            var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        ));
        let mut extra = vec![
            PathBuf::from("/var/lib/flatpak/exports/share"),
            PathBuf::from("/var/lib/snapd/desktop"),
        ];
        extra.extend(
            home.as_ref()
                .map(|h| h.join(".local/share/flatpak/exports/share")),
        );
        for dir in extra {
            if !data.contains(&dir) {
                data.push(dir);
            }
        }
        let mut config = Vec::new();
        config.extend(
            var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| home.map(|h| h.join(".config"))),
        );
        config.extend(list(
            var("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into()),
        ));
        let desktops = var("XDG_CURRENT_DESKTOP")
            .map(|d| d.split(':').map(str::to_lowercase).collect())
            .unwrap_or_default();
        Dirs {
            data,
            config,
            desktops,
        }
    }
}

/// A desktop entry of type Application, as far as Open With needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The `.desktop` file.
    pub path: PathBuf,
    /// Its desktop file ID (`org.gimp.GIMP.desktop`; a subfolder's slash
    /// becomes a dash).
    pub id: String,
    pub name: String,
    pub exec: String,
    pub try_exec: Option<String>,
    pub mime_types: Vec<String>,
    pub icon: Option<String>,
    /// Position of this file's applications directory in the lookup order.
    data_rank: usize,
    raster_editor: bool,
    no_display: bool,
    only_show_in: Option<Vec<String>>,
    not_show_in: Vec<String>,
}

impl Entry {
    /// Whether the launcher can be offered as an Edit With fallback.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn offers_raster_editing(&self, desktops: &[String]) -> bool {
        let matches_desktop = |values: &[String]| {
            values.iter().any(|value| {
                desktops
                    .iter()
                    .any(|desktop| value.eq_ignore_ascii_case(desktop))
            })
        };
        self.raster_editor
            && !self.no_display
            && self.only_show_in.as_deref().is_none_or(matches_desktop)
            && !matches_desktop(&self.not_show_in)
            && takes_files(&self.exec)
    }
}

/// A `Key=value` of the `[Desktop Entry]` group, the key optionally
/// localised (`Name[de]`).
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(';') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The entry of a `.desktop` file's text: an Application that is neither
/// hidden nor a terminal program (`NoDisplay` only hides the launcher; the
/// entry still handles its types). `languages` are the
/// locale names to prefer for `Name`, best first (`de_DE`, `de`).
pub fn parse_entry(text: &str, path: &Path, id: &str, languages: &[String]) -> Option<Entry> {
    let mut in_group = false;
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.entry(key.trim()).or_insert(value.trim_start());
        }
    }
    let flag = |key: &str| fields.get(key).is_some_and(|v| *v == "true");
    if fields.get("Type") != Some(&"Application") || flag("Hidden") || flag("Terminal") {
        return None;
    }
    let exec = unescape(fields.get("Exec")?);
    if exec.trim().is_empty() {
        return None;
    }
    let name = languages
        .iter()
        .find_map(|lang| fields.get(format!("Name[{lang}]").as_str()))
        .or_else(|| fields.get("Name"))
        .map(|n| unescape(n))
        .filter(|n| !n.is_empty())?;
    let list = |key| {
        fields.get(key).map(|value| {
            value
                .split(';')
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
        })
    };
    let categories = list("Categories").unwrap_or_default();
    let raster_editor = categories.contains(&"RasterGraphics")
        || (categories.contains(&"Graphics")
            && categories.contains(&"2DGraphics")
            && list("Keywords").into_iter().flatten().any(|keyword| {
                matches!(
                    keyword
                        .trim()
                        .to_ascii_lowercase()
                        .replace('-', " ")
                        .as_str(),
                    "image editor"
                        | "photo editor"
                        | "raster editor"
                        | "image editing"
                        | "photo editing"
                        | "raster editing"
                )
            }));
    Some(Entry {
        path: path.to_path_buf(),
        id: id.to_string(),
        name,
        exec,
        try_exec: fields.get("TryExec").map(|v| unescape(v)),
        mime_types: list("MimeType")
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        icon: fields.get("Icon").map(|v| unescape(v)),
        data_rank: 1,
        raster_editor,
        no_display: flag("NoDisplay"),
        only_show_in: list("OnlyShowIn").map(|v| v.into_iter().map(str::to_owned).collect()),
        not_show_in: list("NotShowIn")
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    })
}

/// Locale names to prefer from `LC_MESSAGES`/`LANG`-style values
/// (`de_DE.UTF-8@euro` → `de_DE`, `de`).
pub fn languages(locale: &str) -> Vec<String> {
    let base = locale.split(['.', '@']).next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let mut out = vec![base.to_string()];
    if let Some((lang, _)) = base.split_once('_') {
        out.push(lang.to_string());
    }
    out
}

/// Whether `program` (as `TryExec` names it) exists: an absolute file or
/// one found on `path_dirs`.
pub fn program_exists(program: &str, path_dirs: &[PathBuf]) -> bool {
    let p = Path::new(program);
    if p.is_absolute() {
        return is_executable(p);
    }
    path_dirs
        .iter()
        .any(|dir| is_executable(&dir.join(program)))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Every desktop entry under `dirs.data`, a desktop file ID once (the most
/// specific directory wins), subfolders included.
pub fn all_entries(dirs: &Dirs, languages: &[String], path_dirs: &[PathBuf]) -> Vec<Entry> {
    fn walk(base: &Path, folder: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
        let Ok(read) = fs::read_dir(folder) else {
            return;
        };
        let mut items: Vec<_> = read.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for item in items {
            let path = item.path();
            if path.is_dir() {
                if depth < 4 {
                    walk(base, &path, out, depth + 1);
                }
            } else if path.extension().is_some_and(|e| e == "desktop")
                && let Ok(relative) = path.strip_prefix(base)
            {
                let id = relative.to_string_lossy().replace('/', "-");
                out.push((id, path));
            }
        }
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for (rank, data) in dirs.data.iter().enumerate() {
        let base = data.join("applications");
        let mut found = Vec::new();
        walk(&base, &base, &mut found, 0);
        for (id, path) in found {
            // A more specific directory's entry hides the same ID, even
            // when it is hidden itself (how a user removes an entry).
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(mut entry) = parse_entry(&text, &path, &id, languages)
                && entry
                    .try_exec
                    .as_deref()
                    .is_none_or(|t| program_exists(t, path_dirs))
            {
                entry.data_rank = rank + 1;
                entries.push(entry);
            }
        }
    }
    entries
}

/// The MIME types of a file name from the shared MIME database's
/// `globs2` (`weight:type:glob[:flags]`), the best match first: the
/// longest matching glob, then the highest weight.
pub fn mime_types_of(file_name: &str, globs: &[String]) -> Vec<String> {
    let lower = file_name.to_lowercase();
    let mut found: Vec<(usize, u32, String)> = Vec::new();
    for text in globs {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut parts = line.split(':');
            let (Some(weight), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let case_sensitive = parts.next().is_some_and(|f| f.contains("cs"));
            let (name, glob) = if case_sensitive {
                (file_name.to_string(), glob.to_string())
            } else {
                (lower.clone(), glob.to_lowercase())
            };
            let hit = match glob.strip_prefix('*') {
                Some(suffix) if !suffix.contains(['*', '?', '[']) => name.ends_with(suffix),
                None if !glob.contains(['*', '?', '[']) => name == glob,
                _ => false,
            };
            if hit {
                found.push((glob.len(), weight.parse().unwrap_or(50), mime.to_string()));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut seen = HashSet::new();
    found
        .into_iter()
        .map(|(_, _, mime)| mime)
        .filter(|m| seen.insert(m.clone()))
        .collect()
}

/// `mime` with its aliases resolved and its parent types
/// (`subclasses`: `child parent` per line, `aliases`: `alias type`),
/// nearest first. The generic `application/octet-stream` is left out: it
/// would offer every hex editor.
pub fn mime_family(mime: &str, subclasses: &[String], aliases: &[String]) -> Vec<String> {
    let pairs = |texts: &[String]| -> Vec<(String, String)> {
        texts
            .iter()
            .flat_map(|t| t.lines())
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                Some((p.next()?.to_string(), p.next()?.to_string()))
            })
            .collect()
    };
    let subclasses = pairs(subclasses);
    let aliases = pairs(aliases);
    let canonical = aliases
        .iter()
        .find(|(alias, _)| alias == mime)
        .map_or(mime, |(_, real)| real.as_str())
        .to_string();
    let mut family = vec![canonical];
    let mut i = 0;
    while i < family.len() {
        let current = family[i].clone();
        for (child, parent) in &subclasses {
            if *child == current && !family.contains(parent) && parent != "application/octet-stream"
            {
                family.push(parent.clone());
            }
        }
        i += 1;
    }
    family
}

#[derive(Debug, Clone)]
struct Association {
    id: String,
    /// Config files have rank zero, ahead of every applications directory.
    data_rank: usize,
}

/// What `mimeapps.list` files say, merged in precedence order.
#[derive(Debug, Default, Clone)]
pub struct Associations {
    /// Per type, desktop file IDs in preference order.
    default: HashMap<String, Vec<Association>>,
    added: HashMap<String, Vec<Association>>,
    removed: HashMap<String, Vec<Association>>,
}

impl Associations {
    pub fn is_removed(&self, mime: &str, entry: &Entry) -> bool {
        self.removed.get(mime).is_some_and(|associations| {
            associations
                .iter()
                .any(|a| a.id == entry.id && a.data_rank <= entry.data_rank)
        })
    }

    /// Merges one file's text; call it for the most specific file first, an
    /// earlier file's choices come before a later one's.
    fn add_file(&mut self, text: &str, data_rank: usize) {
        // What higher-priority files removed binds this file's additions.
        let removed_before = self.removed.clone();
        let mut section = "";
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                section = line;
                continue;
            }
            let Some((mime, ids)) = line.split_once('=') else {
                continue;
            };
            let mime = mime.trim();
            let (map, blocked) = match section {
                "[Default Applications]" => (&mut self.default, false),
                "[Added Associations]" => (&mut self.added, true),
                "[Removed Associations]" => (&mut self.removed, false),
                _ => continue,
            };
            let list = map.entry(mime.to_string()).or_default();
            for id in ids.split(';').filter(|i| !i.is_empty()) {
                let blocked = blocked
                    && removed_before
                        .get(mime)
                        .is_some_and(|r| r.iter().any(|r| r.id == id));
                if !blocked && !list.iter().any(|a| a.id == id) {
                    list.push(Association {
                        id: id.to_string(),
                        data_rank,
                    });
                }
            }
        }
    }

    /// The files in the specification's lookup order: the desktop-specific
    /// and plain `mimeapps.list` of each config directory, then of each
    /// data directory's `applications`.
    pub fn read(dirs: &Dirs) -> Self {
        let mut out = Associations::default();
        let folders = dirs.config.iter().cloned().map(|dir| (0, dir)).chain(
            dirs.data
                .iter()
                .enumerate()
                .map(|(rank, dir)| (rank + 1, dir.join("applications"))),
        );
        for (data_rank, folder) in folders {
            for desktop in &dirs.desktops {
                if let Ok(text) =
                    fs::read_to_string(folder.join(format!("{desktop}-mimeapps.list")))
                {
                    out.add_file(&text, data_rank);
                }
            }
            if let Ok(text) = fs::read_to_string(folder.join("mimeapps.list")) {
                out.add_file(&text, data_rank);
            }
        }
        out
    }
}

/// The entries that open a file of the types `family` (nearest first):
/// those whose `MimeType` holds a type, and those `[Added Associations]`
/// names, without the ones `[Removed Associations]` takes away. Plus the
/// default entry's ID: the first of `[Default Applications]` for the
/// nearest type that names an installed one.
pub fn apps_for_family<'a>(
    family: &[String],
    entries: &'a [Entry],
    assoc: &Associations,
) -> (Vec<&'a Entry>, Option<String>) {
    let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut out: Vec<&Entry> = Vec::new();
    let mut push = |entry: &'a Entry| {
        if !out.iter().any(|o| o.id == entry.id) {
            out.push(entry);
        }
    };
    let mut default = None;
    for mime in family {
        if default.is_none() {
            default = assoc
                .default
                .get(mime)
                .and_then(|apps| apps.iter().find(|a| by_id.contains_key(a.id.as_str())))
                .map(|a| a.id.clone());
        }
        for addition in assoc.added.get(mime).into_iter().flatten() {
            if let Some(entry) = by_id.get(addition.id.as_str())
                && addition.data_rank <= entry.data_rank
            {
                push(entry);
            }
        }
        for entry in entries {
            if entry.mime_types.iter().any(|m| m == mime) && !assoc.is_removed(mime, entry) {
                push(entry);
            }
        }
    }
    // An entry that does not take files cannot open them.
    out.retain(|e| takes_files(&e.exec));
    (out, default)
}

/// The menu entry of a desktop entry. The id is its `.desktop` file.
pub fn choice(entry: &Entry, icon: Option<String>) -> AppChoice {
    AppChoice {
        path: entry.path.to_string_lossy().into_owned(),
        name: entry.name.clone(),
        is_default: false,
        is_recent: false,
        recommended: true,
        icon,
    }
}

/// A copy of Rawtonic itself (this build, another install or a QA build):
/// its program has our executable's name.
pub fn is_own(entry: &Entry, own_exe: Option<&Path>) -> bool {
    let own = own_exe.and_then(Path::file_name);
    let theirs = split_exec(&entry.exec)
        .and_then(|args| args.into_iter().next())
        .map(PathBuf::from);
    own.is_some() && theirs.as_deref().and_then(Path::file_name) == own
}

/// `Exec` split into arguments by the specification's quoting: arguments
/// are separated by spaces; inside double quotes `\"`, `` \` ``, `\$` and
/// `\\` stand for the character after the backslash. None when a quote is
/// left open.
pub fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' if matches!(chars.peek(), Some('"' | '`' | '$' | '\\')) => {
                    current.push(chars.next()?);
                }
                _ => current.push(c),
            }
        } else {
            match c {
                '"' => {
                    quoted = true;
                    started = true;
                }
                ' ' | '\t' => {
                    if started || !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                _ => current.push(c),
            }
        }
    }
    if quoted {
        return None;
    }
    if started || !current.is_empty() {
        args.push(current);
    }
    Some(args)
}

/// A file field code (`%f %F %u %U`) in an argument, ignoring `%%`.
fn file_codes(arg: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(code) = chars.next()
            && matches!(code, 'f' | 'F' | 'u' | 'U')
        {
            out.push(code);
        }
    }
    out
}

/// Whether `exec` can be given files at all.
pub fn takes_files(exec: &str) -> bool {
    split_exec(exec).is_some_and(|args| args.iter().any(|a| !file_codes(a).is_empty()))
}

/// `file://` URL of an absolute path, bytes outside the unreserved set and
/// `/` percent-encoded.
pub fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The environment changes that give a program started from an AppImage the
/// desktop's environment again: the variables its launcher points into the
/// image are removed, the search paths lose their entries inside it.
/// `(name, None)` removes a variable, `(name, Some(value))` sets it.
pub fn desktop_environment(
    appdir: &str,
    vars: &[(String, String)],
) -> Vec<(String, Option<String>)> {
    const OWN: &[&str] = &[
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "GTK_PATH",
        "GTK_EXEC_PREFIX",
        "GTK_EXE_PREFIX",
        "GTK_DATA_PREFIX",
        "GI_TYPELIB_PATH",
        "GIO_MODULE_DIR",
        "GST_PLUGIN_PATH_1_0",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "GST_PLUGIN_SCANNER_1_0",
        "GTK_IM_MODULE_FILE",
        "GDK_PIXBUF_MODULE_FILE",
        "GDK_PIXBUF_MODULEDIR",
        "GIO_EXTRA_MODULES",
        "GSETTINGS_SCHEMA_DIR",
        "GST_PLUGIN_PATH",
        "GST_PLUGIN_SYSTEM_PATH",
        "GST_PLUGIN_SCANNER",
        "PERLLIB",
        "PYTHONHOME",
        "PYTHONPATH",
        "QT_PLUGIN_PATH",
        "WEBKIT_EXEC_PATH",
    ];
    const PATHS: &[&str] = &["PATH", "XDG_DATA_DIRS", "XDG_CONFIG_DIRS"];
    let mut changes = Vec::new();
    if appdir.is_empty() {
        return changes;
    }
    for (name, value) in vars {
        if OWN.contains(&name.as_str()) && value.contains(appdir) {
            changes.push((name.clone(), None));
        } else if PATHS.contains(&name.as_str()) {
            let kept: Vec<&str> = value
                .split(':')
                .filter(|entry| !entry.starts_with(appdir))
                .collect();
            if kept.len() != value.split(':').count() {
                changes.push((name.clone(), (!kept.is_empty()).then(|| kept.join(":"))));
            }
        }
    }
    changes
}

/// What the field codes of an entry stand for.
pub struct Fields<'a> {
    pub name: &'a str,
    pub icon: Option<&'a str>,
    pub desktop_file: &'a Path,
}

/// The commands that open `files` with `exec`, each a program and its
/// arguments. `%F` and `%U` take all files in one command; with only `%f`
/// or `%u` every file gets a command of its own. `%%` is a percent sign,
/// `%i` the icon option, `%c` the name, `%k` the desktop file; the
/// deprecated codes and unknown ones vanish. A program without a file code
/// is given the files after its arguments (a program file the user picked).
pub fn commands(
    exec: &str,
    files: &[PathBuf],
    fields: &Fields,
) -> Result<Vec<Vec<String>>, String> {
    let template = split_exec(exec).ok_or("The program's command line is not valid.")?;
    if template.is_empty() {
        return Err("The program's command line is empty.".into());
    }
    let codes: Vec<char> = template.iter().flat_map(|a| file_codes(a)).collect();
    let multiple = codes.iter().any(|c| matches!(c, 'F' | 'U'));
    let single = codes.iter().any(|c| matches!(c, 'f' | 'u'));
    let batches: Vec<&[PathBuf]> = if multiple || !single {
        vec![files]
    } else {
        files.chunks(1).collect()
    };
    let mut out = Vec::new();
    for batch in batches {
        let mut args: Vec<String> = Vec::new();
        for arg in &template {
            // A list code standing alone is replaced by every file.
            if arg == "%F" || arg == "%U" {
                for file in batch {
                    args.push(if arg == "%F" {
                        file.to_string_lossy().into_owned()
                    } else {
                        file_url(file)
                    });
                }
                continue;
            }
            if arg == "%i" {
                if let Some(icon) = fields.icon {
                    args.push("--icon".into());
                    args.push(icon.into());
                }
                continue;
            }
            let mut text = String::new();
            let mut chars = arg.chars();
            let mut vanished = false;
            while let Some(c) = chars.next() {
                if c != '%' {
                    text.push(c);
                    continue;
                }
                match chars.next() {
                    Some('%') => text.push('%'),
                    Some('f') => text.push_str(
                        &batch
                            .first()
                            .map_or(String::new(), |f| f.to_string_lossy().into_owned()),
                    ),
                    Some('u') => {
                        text.push_str(&batch.first().map_or(String::new(), |f| file_url(f)))
                    }
                    // Embedded list codes are not valid; the first file
                    // stands for them.
                    Some('F') => text.push_str(
                        &batch
                            .first()
                            .map_or(String::new(), |f| f.to_string_lossy().into_owned()),
                    ),
                    Some('U') => {
                        text.push_str(&batch.first().map_or(String::new(), |f| file_url(f)))
                    }
                    Some('c') => text.push_str(fields.name),
                    Some('k') => text.push_str(&fields.desktop_file.to_string_lossy()),
                    Some(_) | None => vanished = true,
                }
            }
            if vanished && text.is_empty() {
                continue;
            }
            args.push(text);
        }
        if codes.is_empty() {
            args.extend(batch.iter().map(|f| f.to_string_lossy().into_owned()));
        }
        out.push(args);
    }
    Ok(out)
}

/// Icon files of a desktop entry's `Icon` (a name or an absolute path), as a
/// `data:` URL when one is found under `dirs.data` (hicolor and pixmaps) or
/// named by path. Larger than `LIMIT` is left out.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn icon_data_url(icon: &str, dirs: &Dirs) -> Option<String> {
    use base64::Engine;
    const LIMIT: u64 = 256 * 1024;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if Path::new(icon).is_absolute() {
        candidates.push(icon.into());
    } else {
        for data in &dirs.data {
            for size in ["48x48", "64x64", "32x32", "128x128", "256x256", "scalable"] {
                for ext in ["png", "svg"] {
                    candidates.push(
                        data.join("icons/hicolor")
                            .join(size)
                            .join("apps")
                            .join(format!("{icon}.{ext}")),
                    );
                }
            }
            for ext in ["png", "svg"] {
                candidates.push(data.join("pixmaps").join(format!("{icon}.{ext}")));
            }
        }
    }
    candidates.into_iter().find_map(|path| {
        let mime = match path.extension()?.to_str()? {
            "png" => "image/png",
            "svg" => "image/svg+xml",
            _ => return None,
        };
        let meta = fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > LIMIT {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_text(extra: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Editor\nName[de]=Bearbeiter\nExec=editor %F\n{extra}"
        )
    }

    fn parse(text: &str) -> Option<Entry> {
        parse_entry(text, Path::new("/x/editor.desktop"), "editor.desktop", &[])
    }

    #[test]
    fn parses_name_exec_and_types() {
        let e = parse(&entry_text(
            "MimeType=image/tiff;image/jpeg;\nIcon=editor\n",
        ))
        .unwrap();
        assert_eq!(e.name, "Editor");
        assert_eq!(e.exec, "editor %F");
        assert_eq!(e.mime_types, ["image/tiff", "image/jpeg"]);
        assert_eq!(e.icon.as_deref(), Some("editor"));
        let de = parse_entry(
            &entry_text(""),
            Path::new("/a"),
            "a",
            &languages("de_DE.UTF-8"),
        );
        assert_eq!(de.unwrap().name, "Bearbeiter");
    }

    #[test]
    fn skips_what_cannot_open_a_file_for_the_user() {
        for extra in ["Hidden=true\n", "Terminal=true\n"] {
            assert!(parse(&entry_text(extra)).is_none(), "{extra}");
        }
        assert!(parse("[Desktop Entry]\nType=Link\nName=A\nExec=a\n").is_none());
        assert!(parse("[Desktop Entry]\nType=Application\nName=A\n").is_none());
        // Keys of other groups (an action) do not count.
        let text = entry_text("[Desktop Action new]\nName=New\nExec=other\nHidden=true\n");
        assert_eq!(parse(&text).unwrap().exec, "editor %F");
    }

    #[test]
    fn a_launcher_hidden_from_menus_still_handles_its_types() {
        let e = parse(&entry_text("NoDisplay=true\nMimeType=image/tiff;\n")).unwrap();
        assert_eq!(e.mime_types, ["image/tiff"]);
    }

    #[test]
    fn a_removal_of_the_user_beats_an_addition_of_the_system() {
        let mut assoc = Associations::default();
        assoc.add_file("[Removed Associations]\nimage/tiff=paint.desktop;\n", 0);
        assoc.add_file(
            "[Added Associations]\nimage/tiff=paint.desktop;edit.desktop;\n",
            1,
        );
        assert_eq!(assoc.added["image/tiff"].len(), 1);
        assert_eq!(assoc.added["image/tiff"][0].id, "edit.desktop");
        // An addition of the user stays, a lower file's removal does not undo it.
        let mut assoc = Associations::default();
        assoc.add_file("[Added Associations]\nimage/tiff=paint.desktop;\n", 0);
        assoc.add_file("[Removed Associations]\nimage/tiff=paint.desktop;\n", 1);
        assert_eq!(assoc.added["image/tiff"].len(), 1);
        assert_eq!(assoc.added["image/tiff"][0].id, "paint.desktop");
    }

    #[test]
    fn an_appimages_own_environment_is_not_passed_on() {
        let vars = |l: &[(&str, &str)]| -> Vec<(String, String)> {
            l.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let changes = desktop_environment(
            "/tmp/.mount_Raw",
            &vars(&[
                ("LD_LIBRARY_PATH", "/tmp/.mount_Raw/usr/lib"),
                ("GTK_PATH", "/usr/lib/gtk"),
                ("PATH", "/tmp/.mount_Raw/usr/bin:/usr/bin"),
                ("XDG_DATA_DIRS", "/tmp/.mount_Raw/usr/share"),
                ("HOME", "/home/u"),
            ]),
        );
        assert_eq!(
            changes,
            [
                ("LD_LIBRARY_PATH".to_string(), None),
                ("PATH".to_string(), Some("/usr/bin".to_string())),
                ("XDG_DATA_DIRS".to_string(), None),
            ]
        );
        assert!(desktop_environment("", &vars(&[("LD_LIBRARY_PATH", "/x")])).is_empty());
    }

    #[test]
    fn unescapes_values() {
        let e = parse("[Desktop Entry]\nType=Application\nName=A\\sB\nExec=a\\\\b %f\n").unwrap();
        assert_eq!(e.name, "A B");
        assert_eq!(e.exec, "a\\b %f");
    }

    #[test]
    fn splits_exec_by_the_quoting_rules() {
        assert_eq!(
            split_exec(r#"/opt/My App/run --x "a b" 'c' "q\"d" "\\e" "\$f""#).unwrap(),
            [
                "/opt/My", "App/run", "--x", "a b", "'c'", "q\"d", "\\e", "$f"
            ]
        );
        assert_eq!(
            split_exec(r#""/opt/My App/run" %F"#).unwrap(),
            ["/opt/My App/run", "%F"]
        );
        assert_eq!(split_exec(r#"a "" b"#).unwrap(), ["a", "", "b"]);
        assert!(split_exec(r#"a "unclosed"#).is_none());
    }

    fn fields() -> Fields<'static> {
        Fields {
            name: "Editor",
            icon: Some("editor"),
            desktop_file: Path::new("/usr/share/applications/editor.desktop"),
        }
    }

    fn files() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/p/Größe A.tiff"),
            PathBuf::from("/p/b;rm -rf $HOME`x`.tiff"),
        ]
    }

    #[test]
    fn list_codes_take_every_file_as_its_own_argument() {
        let cmds = commands(r#""/opt/My App/run" --new %F"#, &files(), &fields()).unwrap();
        assert_eq!(
            cmds,
            [[
                "/opt/My App/run",
                "--new",
                "/p/Größe A.tiff",
                "/p/b;rm -rf $HOME`x`.tiff"
            ]]
        );
        let urls = commands("editor %U", &files(), &fields()).unwrap();
        assert_eq!(
            urls[0],
            [
                "editor",
                "file:///p/Gr%C3%B6%C3%9Fe%20A.tiff",
                "file:///p/b%3Brm%20-rf%20%24HOME%60x%60.tiff"
            ]
        );
    }

    #[test]
    fn single_codes_start_one_command_per_file() {
        let cmds = commands("editor --open=%f", &files(), &fields()).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], ["editor", "--open=/p/Größe A.tiff"]);
        assert_eq!(cmds[1][1], "--open=/p/b;rm -rf $HOME`x`.tiff");
    }

    #[test]
    fn other_codes_are_expanded_or_dropped() {
        let cmds = commands(
            "editor %i %c %k 100%% %d %D %n %v %m %F",
            &files()[..1],
            &fields(),
        )
        .unwrap();
        assert_eq!(
            cmds[0],
            [
                "editor",
                "--icon",
                "editor",
                "Editor",
                "/usr/share/applications/editor.desktop",
                "100%",
                "/p/Größe A.tiff"
            ]
        );
        // `%%f` is a percent sign and an f, not a file.
        assert_eq!(
            commands("editor %%f %f", &files()[..1], &fields()).unwrap()[0],
            ["editor", "%f", "/p/Größe A.tiff"]
        );
    }

    #[test]
    fn a_program_without_field_codes_is_given_the_files() {
        let cmds = commands("/usr/bin/tool --flag", &files(), &fields()).unwrap();
        assert_eq!(cmds[0][..2], ["/usr/bin/tool", "--flag"]);
        assert_eq!(cmds[0].len(), 4);
        assert!(!takes_files("tool --flag"));
        assert!(takes_files("tool %%f %U"));
        assert!(!takes_files("tool %%f"));
    }

    #[test]
    fn flatpak_file_forwarding_markers_stay_around_the_files() {
        let cmds = commands(
            "/usr/bin/flatpak run --command=gimp org.gimp.GIMP @@u %U @@",
            &files()[..1],
            &fields(),
        )
        .unwrap();
        assert_eq!(
            cmds[0][4..],
            ["@@u", "file:///p/Gr%C3%B6%C3%9Fe%20A.tiff", "@@"]
        );
    }

    fn glob_db() -> Vec<String> {
        vec![
            "# comment\n50:image/tiff:*.tif\n50:image/tiff:*.tiff\n50:image/x-nikon-nef:*.nef\n\
             50:image/x-adobe-dng:*.dng\n50:image/jpeg:*.jpg\n50:image/jpeg:*.jpeg\n\
             50:application/x-compressed-tar:*.tar.gz\n50:application/gzip:*.gz\n\
             50:text/x-makefile:Makefile:cs\n"
                .into(),
        ]
    }

    #[test]
    fn mime_type_follows_the_longest_matching_glob() {
        let g = glob_db();
        assert_eq!(mime_types_of("A.NEF", &g), ["image/x-nikon-nef"]);
        assert_eq!(
            mime_types_of("x.tar.gz", &g)[0],
            "application/x-compressed-tar"
        );
        assert_eq!(mime_types_of("Makefile", &g), ["text/x-makefile"]);
        assert!(mime_types_of("makefile", &g).is_empty());
        assert!(mime_types_of("a.unknown", &g).is_empty());
    }

    #[test]
    fn mime_family_resolves_aliases_and_parents_but_not_octet_stream() {
        let subclasses = vec![
            "image/x-nikon-nef image/x-dcraw\nimage/x-dcraw application/octet-stream\n\
             image/x-adobe-dng image/tiff\n"
                .to_string(),
        ];
        let aliases = vec!["image/x-nef image/x-nikon-nef\n".to_string()];
        assert_eq!(
            mime_family("image/x-nef", &subclasses, &aliases),
            ["image/x-nikon-nef", "image/x-dcraw"]
        );
        assert_eq!(
            mime_family("image/x-adobe-dng", &subclasses, &aliases),
            ["image/x-adobe-dng", "image/tiff"]
        );
    }

    fn entry(id: &str, name: &str, mimes: &[&str]) -> Entry {
        Entry {
            path: PathBuf::from(format!("/usr/share/applications/{id}")),
            id: id.into(),
            name: name.into(),
            exec: "app %F".into(),
            try_exec: None,
            mime_types: mimes.iter().map(|m| m.to_string()).collect(),
            icon: None,
            data_rank: 1,
            raster_editor: false,
            no_display: false,
            only_show_in: None,
            not_show_in: Vec::new(),
        }
    }

    #[test]
    fn lists_apps_of_the_family_with_the_default_and_associations() {
        let entries = vec![
            entry("gimp.desktop", "GIMP", &["image/tiff", "image/jpeg"]),
            entry("viewer.desktop", "Viewer", &["image/jpeg"]),
            entry("raw.desktop", "Raw Tool", &["image/x-dcraw"]),
            entry("extra.desktop", "Extra", &[]),
            entry("removed.desktop", "Removed", &["image/tiff"]),
        ];
        let mut assoc = Associations::default();
        assoc.add_file(
            "[Default Applications]\nimage/tiff=missing.desktop;gimp.desktop;\n\
             [Added Associations]\nimage/tiff=extra.desktop;\n\
             [Removed Associations]\nimage/tiff=removed.desktop;\n",
            0,
        );
        // A later (less specific) file does not override an earlier choice.
        assoc.add_file("[Default Applications]\nimage/tiff=viewer.desktop\n", 1);
        let family = vec!["image/tiff".to_string(), "image/x-dcraw".to_string()];
        let (apps, default) = apps_for_family(&family, &entries, &assoc);
        let ids: Vec<_> = apps.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["extra.desktop", "gimp.desktop", "raw.desktop"]);
        assert_eq!(default.as_deref(), Some("gimp.desktop"));
    }

    #[test]
    fn a_system_mime_removal_does_not_hide_a_user_desktop_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let system = tmp.path().join("system");
        for dir in [&user, &system] {
            fs::create_dir_all(dir.join("applications")).unwrap();
            rawtonic_fs::atomic_write(
                &dir.join("applications/editor.desktop"),
                entry_text("MimeType=image/tiff;\n"),
            )
            .unwrap();
        }
        rawtonic_fs::atomic_write(
            &system.join("applications/other.desktop"),
            entry_text("MimeType=image/tiff;\n"),
        )
        .unwrap();
        // XDG MIME Apps §3 scopes a removal to desktop files in the same
        // directory or a directory later in the lookup order.
        rawtonic_fs::atomic_write(
            &system.join("applications/mimeapps.list"),
            "[Removed Associations]\nimage/tiff=editor.desktop;other.desktop;\n\
             [Added Associations]\nimage/png=editor.desktop;\n",
        )
        .unwrap();
        let dirs = Dirs {
            data: vec![user.clone(), system],
            ..Dirs::default()
        };
        let entries = all_entries(&dirs, &[], &[]);
        let (apps, _) =
            apps_for_family(&["image/tiff".into()], &entries, &Associations::read(&dirs));
        let paths: Vec<_> = apps.iter().map(|app| &app.path).collect();
        assert_eq!(paths, [&user.join("applications/editor.desktop")]);
        let (apps, _) =
            apps_for_family(&["image/png".into()], &entries, &Associations::read(&dirs));
        assert!(
            apps.is_empty(),
            "a lower-priority addition also stays local"
        );

        rawtonic_fs::atomic_write(
            &user.join("applications/mimeapps.list"),
            "[Removed Associations]\nimage/tiff=editor.desktop;\n",
        )
        .unwrap();
        let (apps, _) =
            apps_for_family(&["image/tiff".into()], &entries, &Associations::read(&dirs));
        assert!(apps.is_empty(), "the user's removal still applies");
    }

    #[test]
    fn reads_entries_from_the_directories_most_specific_first() {
        let tmp = tempfile::tempdir().unwrap();
        let (user, system) = (tmp.path().join("user"), tmp.path().join("sys"));
        for (dir, name) in [(&user, "User Editor"), (&system, "System Editor")] {
            fs::create_dir_all(dir.join("applications/sub")).unwrap();
            fs::write(
                dir.join("applications/editor.desktop"),
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec=editor %f\n"),
            )
            .unwrap();
        }
        fs::write(
            system.join("applications/sub/nested.desktop"),
            "[Desktop Entry]\nType=Application\nName=Nested\nExec=nested %f\n",
        )
        .unwrap();
        fs::write(
            system.join("applications/gone.desktop"),
            "[Desktop Entry]\nType=Application\nName=Gone\nExec=gone %f\nTryExec=/no/such/program\n",
        )
        .unwrap();
        fs::write(
            user.join("applications/mimeapps.list"),
            "[Default Applications]\nimage/tiff=editor.desktop\n",
        )
        .unwrap();
        let dirs = Dirs {
            data: vec![user.clone(), system],
            config: vec![],
            desktops: vec![],
        };
        let entries = all_entries(&dirs, &[], &[]);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["User Editor", "Nested"]);
        assert_eq!(entries[1].id, "sub-nested.desktop");
        let assoc = Associations::read(&dirs);
        assert_eq!(assoc.default["image/tiff"].len(), 1);
        assert_eq!(assoc.default["image/tiff"][0].id, "editor.desktop");
    }

    #[test]
    fn a_hidden_entry_of_the_user_hides_the_system_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (user, system) = (tmp.path().join("user"), tmp.path().join("sys"));
        for dir in [&user, &system] {
            fs::create_dir_all(dir.join("applications")).unwrap();
        }
        fs::write(
            user.join("applications/a.desktop"),
            "[Desktop Entry]\nType=Application\nName=A\nExec=a\nHidden=true\n",
        )
        .unwrap();
        fs::write(
            system.join("applications/a.desktop"),
            "[Desktop Entry]\nType=Application\nName=A\nExec=a %f\n",
        )
        .unwrap();
        let dirs = Dirs {
            data: vec![user, system],
            ..Dirs::default()
        };
        assert!(all_entries(&dirs, &[], &[]).is_empty());
    }

    #[test]
    fn own_copies_are_recognised_by_program_name() {
        let mut e = entry("rt.desktop", "Rawtonic", &["image/tiff"]);
        e.exec = "/opt/Rawtonic/rawtonic %F".into();
        assert!(is_own(&e, Some(Path::new("/home/u/app/rawtonic"))));
        assert!(!is_own(&e, Some(Path::new("/usr/bin/gimp"))));
    }

    #[test]
    fn language_candidates() {
        assert_eq!(languages("de_DE.UTF-8@euro"), ["de_DE", "de"]);
        assert_eq!(languages("fr"), ["fr"]);
        assert!(languages("C").is_empty());
    }
}

//! Rename planning: one plan for the rename dialog's preview and for the
//! rename itself. It holds every photo's final name, each file that goes
//! with it (editor sidecar, virtual copies, EXIF cache, XMP, companion
//! JPEG) and what stops it; the rename runs exactly the plan the dialog
//! showed (`rename_files`), the journal checking no-clobber again.
//! Part of `file_management` (ADR 0009).
use super::parse_virtual_path;
use crate::formats::{is_raw_file, is_supported_image_file};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What a planned file is to its photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileRole {
    Photo,
    EditSidecar,
    VirtualCopy,
    ExifCache,
    Xmp,
    Companion,
}

/// One file renamed: `from` → `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFile {
    pub from: PathBuf,
    pub to: PathBuf,
    pub role: FileRole,
}

/// Something the user should know before renaming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Note {
    /// The XMP shared with photos left out of the rename (`others`) is
    /// renamed with this RAW; they keep their names without it.
    XmpWithRaw { xmp: PathBuf, others: Vec<PathBuf> },
    /// The XMP stays with its RAW (`owner`), which is not renamed.
    XmpStays { xmp: PathBuf, owner: PathBuf },
}

/// Why a photo cannot be renamed; any problem stops the whole rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Problem {
    NotFound,
    InvalidName {
        name: String,
    },
    /// A file is already at `path`.
    Occupied {
        path: PathBuf,
    },
    /// Another file of this rename would get the name `path`.
    SameTarget {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedPhoto {
    pub from: PathBuf,
    /// Its final name (`from` when it keeps its name).
    pub to: PathBuf,
    /// Every file renamed with it, the photo first; empty when unchanged.
    pub files: Vec<PlannedFile>,
    pub notes: Vec<Note>,
    pub problems: Vec<Problem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlan {
    /// The photos in the order given, each once (virtual copies are their photo).
    pub photos: Vec<PlannedPhoto>,
}

impl RenamePlan {
    pub fn blocked(&self) -> bool {
        self.photos.iter().any(|p| !p.problems.is_empty())
    }

    pub fn files(&self) -> Vec<PlannedFile> {
        self.photos.iter().flat_map(|p| p.files.clone()).collect()
    }

    /// `path` (a photo or a virtual copy, `<photo>?vc=<id>`) after the rename.
    pub fn renamed(&self, path: &str) -> String {
        let (photo, _) = parse_virtual_path(path);
        match (
            self.photos.iter().find(|p| p.from == photo),
            path.rsplit_once("?vc="),
        ) {
            (Some(p), Some((_, id))) => format!("{}?vc={id}", p.to.to_string_lossy()),
            (Some(p), None) => p.to.to_string_lossy().into_owned(),
            (None, _) => path.to_string(),
        }
    }
}

/// How `entry` (a file name beside the photo named `photo`) belongs to
/// it: its editor sidecar, EXIF cache or a virtual copy's sidecar
/// (`<photo>.<copy id>.rrdata`). The sidecar of another photo whose name
/// starts with this one's (`A.NEF.jpg.rrdata` of a derivative `A.NEF.jpg`)
/// is not its own.
pub(crate) fn own_file_role(photo: &str, entry: &str) -> Option<FileRole> {
    let rest = entry.strip_prefix(photo)?.strip_prefix('.')?;
    match rest {
        "rrdata" => Some(FileRole::EditSidecar),
        "rrexif" => Some(FileRole::ExifCache),
        _ => {
            let id = rest.strip_suffix(".rrdata")?;
            // Copy ids never hold a dot nor make the name another photo's.
            (!id.is_empty()
                && !id.contains('.')
                && !is_supported_image_file(format!("{photo}.{id}")))
            .then_some(FileRole::VirtualCopy)
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn folder_entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A batch of several names without a counter or original name would
/// give every photo the same name: it is numbered.
fn effective_template(template: &str, names: usize) -> String {
    let template = template.trim();
    let numbered = rawtonic_naming::template_tokens(template)
        .any(|t| matches!(t, "seq" | "orig" | "orig_base" | "orig_num"));
    if names > 1 && !numbered {
        format!("{template}_{{seq:auto}}")
    } else {
        template.to_string()
    }
}

/// What the plan reads about photos, each once: a verified companion
/// (its whole content hashed), the capture date and, for templates with
/// camera or metadata tokens, its facts with the library's metadata
/// (`naming_capture`). The rename dialog
/// keeps them while it is open (`preview_rename`); the rename itself
/// reads them again.
#[derive(Default)]
pub struct PhotoFacts {
    companions: HashMap<PathBuf, Option<PathBuf>>,
    dates: HashMap<PathBuf, DateTime<Utc>>,
    captures: HashMap<PathBuf, rawtonic_naming::Capture>,
}

impl PhotoFacts {
    fn companion(&mut self, photo: &Path) -> Option<PathBuf> {
        self.companions
            .entry(photo.to_path_buf())
            .or_insert_with(|| crate::session_bridge::companion_of(photo))
            .clone()
    }

    fn date(&mut self, photo: &Path) -> DateTime<Utc> {
        *self
            .dates
            .entry(photo.to_path_buf())
            .or_insert_with(|| crate::exif_processing::get_creation_date_from_path(photo))
    }

    fn capture(&mut self, photo: &Path) -> &rawtonic_naming::Capture {
        self.captures.entry(photo.to_path_buf()).or_insert_with(|| {
            crate::export_processing::naming_capture(
                photo,
                crate::session_bridge::catalog_metadata(photo).as_ref(),
            )
        })
    }
}

/// `name` without `photo`'s extension when it ends in it (any case), as a
/// template with `{orig}` or `.{ext}` gives it: each file of the photo keeps
/// its own extension, appended to the stem (as an import keeps it,
/// `rawtonic_ingest::render_name`).
fn without_extension(name: String, photo: &Path) -> String {
    let Some(ext) = photo.extension().and_then(|e| e.to_str()) else {
        return name;
    };
    let cut = name.len().saturating_sub(ext.len() + 1);
    match name.get(cut..) {
        Some(tail) if cut > 0 && tail.starts_with('.') && tail[1..].eq_ignore_ascii_case(ext) => {
            name[..cut].to_string()
        }
        _ => name,
    }
}

/// The naming tokens of the capture date.
const DATE_TOKENS: [&str; 13] = [
    "yyyy", "yy", "mm", "month", "mon", "dd", "doy", "weekday", "hh", "MM", "ss", "date", "time",
];

/// Plans renaming `paths` (photos or virtual copies) after `template`,
/// reading every fact fresh.
#[cfg(test)]
pub fn plan_rename(paths: &[String], template: &str) -> RenamePlan {
    plan_rename_with(paths, template, 1, &mut PhotoFacts::default())
}

/// Plans renaming `paths` (photos or virtual copies) after `template`.
/// Photos of one name in one folder (independent RAW and JPEG sharing an
/// XMP) are one name in the sequence and keep sharing a name; its date
/// is the RAW's. A verified companion is renamed with its photo. The
/// template uses the naming tokens of ingest and export (`rawtonic_naming`),
/// its counter starting at `sequence_start`.
pub fn plan_rename_with(
    paths: &[String],
    template: &str,
    sequence_start: u64,
    facts: &mut PhotoFacts,
) -> RenamePlan {
    let mut photos: Vec<PathBuf> = Vec::new();
    for path in paths {
        let (photo, _) = parse_virtual_path(path);
        if !photos.contains(&photo) {
            photos.push(photo);
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<(Option<PathBuf>, String), usize> = HashMap::new();
    for (i, photo) in photos.iter().enumerate() {
        let key = (photo.parent().map(Path::to_path_buf), stem(photo));
        let g = *group_of.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    let template = effective_template(template, groups.len());
    let mut planned: Vec<PlannedPhoto> = photos
        .iter()
        .map(|photo| PlannedPhoto {
            from: photo.clone(),
            to: photo.clone(),
            files: Vec::new(),
            notes: Vec::new(),
            problems: Vec::new(),
        })
        .collect();
    for (g, members) in groups.iter().enumerate() {
        let lead = *members
            .iter()
            .find(|&&i| is_raw_file(&photos[i]))
            .unwrap_or(&members[0]);
        let lead_path = &photos[lead];
        let mut naming =
            rawtonic_naming::Context::new(file_name(lead_path), sequence_start + g as u64);
        naming.sequence_start = sequence_start;
        naming.sequence_last = Some(sequence_start + groups.len() as u64 - 1);
        if let Some(folder) = lead_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        {
            naming.values.insert("folder".into(), folder.into());
        }
        if rawtonic_naming::needs_capture(&template) {
            naming.apply_capture(facts.capture(lead_path));
        }
        if rawtonic_naming::template_tokens(&template).any(|t| DATE_TOKENS.contains(&t)) {
            naming.captured = facts
                .date(lead_path)
                .with_timezone(&chrono::Local)
                .naive_local();
        }
        let rendered = rawtonic_naming::render(&template, &naming);
        let invalid = rendered.is_err();
        let new_stem = rendered
            .map(|name| without_extension(name, lead_path))
            .unwrap_or_else(|_| template.clone());
        let folder = lead_path.parent().unwrap_or(Path::new("")).to_path_buf();
        let entries = folder_entries(&folder);
        for &i in members {
            let p = &mut planned[i];
            if !p.from.is_file() {
                p.problems.push(Problem::NotFound);
                continue;
            }
            if invalid {
                p.problems.push(Problem::InvalidName {
                    name: new_stem.clone(),
                });
                continue;
            }
            let renamed = |path: &Path| match path.extension() {
                Some(ext) => folder.join(format!("{new_stem}.{}", ext.to_string_lossy())),
                None => folder.join(&new_stem),
            };
            p.to = renamed(&p.from);
            if p.to == p.from {
                continue;
            }
            let (old_name, new_name) = (file_name(&p.from), file_name(&p.to));
            p.files.push(PlannedFile {
                from: p.from.clone(),
                to: p.to.clone(),
                role: FileRole::Photo,
            });
            for entry in &entries {
                if let Some(role) = own_file_role(&old_name, entry) {
                    p.files.push(PlannedFile {
                        from: folder.join(entry),
                        to: folder.join(format!("{new_name}{}", &entry[old_name.len()..])),
                        role,
                    });
                }
            }
            if let Some(companion) = facts.companion(&p.from) {
                p.files.push(PlannedFile {
                    to: renamed(&companion),
                    from: companion,
                    role: FileRole::Companion,
                });
            }
        }
        if invalid || members.iter().all(|&i| planned[i].files.is_empty()) {
            continue;
        }
        plan_xmp(&mut planned, members, &folder, &entries, &new_stem, facts);
    }
    find_conflicts(&mut planned);
    RenamePlan { photos: planned }
}

/// The XMP of a name group (`<stem>.xmp`, any letter case) is renamed with
/// the group when every photo of that name is in it; otherwise it follows
/// the RAW of that name, and the plan says so.
fn plan_xmp(
    planned: &mut [PlannedPhoto],
    members: &[usize],
    folder: &Path,
    entries: &[String],
    new_stem: &str,
    facts: &mut PhotoFacts,
) {
    let old_stem = stem(&planned[members[0]].from);
    let Some(xmp) = entries.iter().map(|e| folder.join(e)).find(|p| {
        stem(p) == old_stem && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
    }) else {
        return;
    };
    let selected: Vec<&PathBuf> = members.iter().map(|&i| &planned[i].from).collect();
    // Photos of that name; a RAW's companion is no photo of its own.
    let named: Vec<PathBuf> = entries
        .iter()
        .map(|e| folder.join(e))
        .filter(|p| stem(p) == old_stem && is_supported_image_file(p))
        .collect();
    let hidden: Vec<PathBuf> = named
        .iter()
        .filter(|p| is_raw_file(p))
        .filter_map(|p| facts.companion(p))
        .collect();
    let owners: Vec<PathBuf> = named.into_iter().filter(|p| !hidden.contains(p)).collect();
    let others: Vec<PathBuf> = owners
        .iter()
        .filter(|o| !selected.contains(o))
        .cloned()
        .collect();
    let raw = owners.iter().find(|o| is_raw_file(o));
    let to = folder.join(format!(
        "{new_stem}.{}",
        xmp.extension().unwrap_or_default().to_string_lossy()
    ));
    let taker = if others.is_empty() {
        // The RAW's, or the first renamed photo's.
        members
            .iter()
            .copied()
            .filter(|&i| !planned[i].files.is_empty())
            .find(|&i| raw == Some(&planned[i].from))
            .or_else(|| {
                members
                    .iter()
                    .copied()
                    .find(|&i| !planned[i].files.is_empty())
            })
    } else {
        members
            .iter()
            .copied()
            .find(|&i| raw == Some(&planned[i].from) && !planned[i].files.is_empty())
    };
    match taker {
        Some(i) => {
            planned[i].files.push(PlannedFile {
                from: xmp.clone(),
                to,
                role: FileRole::Xmp,
            });
            if !others.is_empty() {
                planned[i].notes.push(Note::XmpWithRaw { xmp, others });
            }
        }
        None => {
            let owner = raw.cloned().unwrap_or_else(|| others[0].clone());
            for &i in members {
                if !planned[i].files.is_empty() {
                    planned[i].notes.push(Note::XmpStays {
                        xmp: xmp.clone(),
                        owner: owner.clone(),
                    });
                }
            }
        }
    }
}

/// Targets taken on disk (other than by the file itself, a case-only
/// rename) or by two files of this rename.
fn find_conflicts(planned: &mut [PlannedPhoto]) {
    let mut count: HashMap<PathBuf, usize> = HashMap::new();
    for file in planned.iter().flat_map(|p| &p.files) {
        *count.entry(file.to.clone()).or_default() += 1;
    }
    for photo in planned.iter_mut() {
        let mut problems = Vec::new();
        for file in &photo.files {
            if count[&file.to] > 1 {
                problems.push(Problem::SameTarget {
                    path: file.to.clone(),
                });
            } else if fs::symlink_metadata(&file.to).is_ok()
                && !rawtonic_fileops::same_file(&file.from, &file.to)
            {
                problems.push(Problem::Occupied {
                    path: file.to.clone(),
                });
            }
        }
        photo.problems.extend(problems);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::{Library, Mode};

    fn session(root: &Path) -> (Library, PathBuf) {
        let dir = root.join("Job");
        let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
        let capture = dir.canonicalize().unwrap().join("Capture");
        (lib, capture)
    }

    fn write(folder: &Path, names: &[&str]) {
        for name in names {
            fs::write(folder.join(name), name).unwrap();
        }
    }

    fn arg(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// The plan's renames as file names, `from → to`.
    fn renames(plan: &RenamePlan) -> Vec<String> {
        let mut names: Vec<String> = plan
            .files()
            .iter()
            .map(|f| format!("{} → {}", file_name(&f.from), file_name(&f.to)))
            .collect();
        names.sort();
        names
    }

    fn names_in(folder: &Path) -> Vec<String> {
        folder_entries(folder)
    }

    /// R1: a derivative `A.NEF.jpg` beside `A.NEF` keeps its own edit
    /// sidecar when `A.NEF` is renamed; `A.NEF`'s virtual copy goes with it.
    #[test]
    fn a_derivatives_sidecar_stays_with_the_derivative() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        write(
            &dir,
            &[
                "A.NEF",
                "A.NEF.rrdata",
                "A.NEF.rrexif",
                "A.NEF.a1b2c3.rrdata",
                "A.NEF.jpg",
                "A.NEF.jpg.rrdata",
                "A.NEF.jpg.d4e5f6.rrdata",
            ],
        );
        let plan = plan_rename(&[arg(&dir.join("A.NEF"))], "Job");
        assert!(!plan.blocked(), "{plan:?}");
        assert_eq!(
            renames(&plan),
            [
                "A.NEF → Job.NEF",
                "A.NEF.a1b2c3.rrdata → Job.NEF.a1b2c3.rrdata",
                "A.NEF.rrdata → Job.NEF.rrdata",
                "A.NEF.rrexif → Job.NEF.rrexif",
            ]
        );
        // Moves and the Trash collect the same files.
        let mut moved: Vec<String> =
            crate::file_management::find_all_associated_files(&dir.join("A.NEF"))
                .unwrap()
                .iter()
                .map(|p| file_name(p))
                .collect();
        moved.sort();
        assert_eq!(
            moved,
            [
                "A.NEF",
                "A.NEF.a1b2c3.rrdata",
                "A.NEF.rrdata",
                "A.NEF.rrexif"
            ]
        );
    }

    #[test]
    fn copy_ids_are_never_another_photos_name() {
        assert_eq!(
            own_file_role("A.NEF", "A.NEF.rrdata"),
            Some(FileRole::EditSidecar)
        );
        assert_eq!(
            own_file_role("A.NEF", "A.NEF.rrexif"),
            Some(FileRole::ExifCache)
        );
        assert_eq!(
            own_file_role("A.NEF", "A.NEF.9b8208.rrdata"),
            Some(FileRole::VirtualCopy)
        );
        for other in [
            "A.NEF.jpg.rrdata",
            "A.NEF.TIF.rrdata",
            "A.NEF.jpg.9b8208.rrdata",
            "A.NEF..rrdata",
            "A.NEFX.rrdata",
            "A.NEF.xmp",
            "A.NEF",
        ] {
            assert_eq!(own_file_role("A.NEF", other), None, "{other}");
        }
    }

    /// R2: independent `A.NEF` and `A.JPG` sharing `A.xmp`, renamed
    /// together, share one sequence number and their XMP follows once.
    #[test]
    fn a_raw_and_jpeg_sharing_an_xmp_are_renamed_as_a_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        write(&dir, &["A.NEF", "A.JPG", "A.xmp", "B.NEF"]);
        let paths = [dir.join("A.NEF"), dir.join("A.JPG"), dir.join("B.NEF")].map(|p| arg(&p));
        let plan = plan_rename(&paths, "Job");
        assert!(!plan.blocked(), "{plan:?}");
        assert_eq!(
            renames(&plan),
            [
                "A.JPG → Job_1.JPG",
                "A.NEF → Job_1.NEF",
                "A.xmp → Job_1.xmp",
                "B.NEF → Job_2.NEF",
            ]
        );
        assert!(plan.photos.iter().all(|p| p.notes.is_empty()));
        // Only one renamed: the XMP follows the RAW, and the plan says so.
        let plan = plan_rename(&[arg(&dir.join("A.NEF"))], "Job");
        assert_eq!(renames(&plan), ["A.NEF → Job.NEF", "A.xmp → Job.xmp"]);
        assert_eq!(
            plan.photos[0].notes,
            [Note::XmpWithRaw {
                xmp: dir.join("A.xmp"),
                others: vec![dir.join("A.JPG")]
            }]
        );
        let plan = plan_rename(&[arg(&dir.join("A.JPG"))], "Job");
        assert_eq!(renames(&plan), ["A.JPG → Job.JPG"]);
        assert_eq!(
            plan.photos[0].notes,
            [Note::XmpStays {
                xmp: dir.join("A.xmp"),
                owner: dir.join("A.NEF")
            }]
        );
    }

    /// R3: a RAW's verified companion JPEG (and the XMP) is renamed with
    /// it; a taken companion name stops the rename.
    #[test]
    fn a_companion_is_renamed_with_its_photo() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, capture) = session(tmp.path());
        write(&capture, &["A.NEF", "A.JPG", "A.xmp", "A.NEF.rrdata"]);
        let planned = lib
            .plan_add(&capture.join("A.NEF"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        lib.insert_planned(vec![planned]).unwrap();
        let plan = plan_rename(&[arg(&capture.join("A.NEF"))], "Job");
        assert!(!plan.blocked(), "{plan:?}");
        assert_eq!(
            renames(&plan),
            [
                "A.JPG → Job.JPG",
                "A.NEF → Job.NEF",
                "A.NEF.rrdata → Job.NEF.rrdata",
                "A.xmp → Job.xmp",
            ]
        );
        assert!(
            plan.photos[0].notes.is_empty(),
            "a companion is no photo of its own"
        );
        write(&capture, &["Job.JPG"]);
        let plan = plan_rename(&[arg(&capture.join("A.NEF"))], "Job");
        assert!(plan.blocked());
        assert_eq!(
            plan.photos[0].problems,
            [Problem::Occupied {
                path: capture.join("Job.JPG")
            }]
        );
    }

    /// R4: the plan holds the final names the dialog shows: the sequence a
    /// batch gets, its width, and why a name cannot be taken.
    #[test]
    fn the_plan_shows_final_names_and_what_stops_them() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let names: Vec<String> = (1..=10).map(|i| format!("P{i}.NEF")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        write(&dir, &refs);
        let paths: Vec<String> = names.iter().map(|n| arg(&dir.join(n))).collect();
        let plan = plan_rename(&paths, " Job ");
        let finals: Vec<String> = plan.photos.iter().map(|p| file_name(&p.to)).collect();
        assert_eq!(finals[0], "Job_01.NEF");
        assert_eq!(finals[9], "Job_10.NEF");
        // A virtual copy is its photo, once; numbering has no gap.
        let with_copy = [
            format!("{}?vc=abc123", paths[0]),
            paths[0].clone(),
            paths[1].clone(),
        ];
        let plan = plan_rename(&with_copy, "Job");
        assert_eq!(plan.photos.len(), 2);
        assert_eq!(
            plan.renamed(&with_copy[0]),
            arg(&dir.join("Job_1.NEF?vc=abc123"))
        );
        assert_eq!(file_name(&plan.photos[1].to), "Job_2.NEF");
        // One photo, a literal name: no sequence.
        let plan = plan_rename(&paths[..1], "Job");
        assert_eq!(file_name(&plan.photos[0].to), "Job.NEF");
        // Unchanged names plan nothing.
        let plan = plan_rename(&paths[..2], "{orig_base}");
        assert!(plan.files().is_empty() && !plan.blocked());
        // Taken, doubled and invalid names.
        let plan = plan_rename(&paths[..1], "P2");
        assert_eq!(
            plan.photos[0].problems,
            [Problem::Occupied {
                path: dir.join("P2.NEF")
            }]
        );
        // Two files of one rename to one name (the journal refuses it too).
        let mut doubled = plan_rename(&paths[..2], "Job").photos;
        doubled[1].files[0].to = doubled[0].files[0].to.clone();
        for p in &mut doubled {
            p.problems.clear();
        }
        find_conflicts(&mut doubled);
        assert!(doubled.iter().all(|p| p.problems
            == [Problem::SameTarget {
                path: dir.join("Job_1.NEF")
            }]));
        let plan = plan_rename(&paths[..1], "a/b");
        assert_eq!(
            plan.photos[0].problems,
            [Problem::InvalidName { name: "a/b".into() }]
        );
        fs::remove_file(dir.join("P1.NEF")).unwrap();
        assert_eq!(
            plan_rename(&paths[..1], "X").photos[0].problems,
            [Problem::NotFound]
        );
    }

    /// No refusals by a case-only change of the same file.
    /// Metadata tokens read what the photographer set in the library, not
    /// only what the camera wrote.
    #[test]
    fn metadata_tokens_come_from_the_library() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, capture) = session(tmp.path());
        write(&capture, &["A.NEF", "B.NEF"]);
        lib.add_files(&[capture.join("A.NEF"), capture.join("B.NEF")])
            .unwrap();
        let id = lib.asset_at(&capture.join("A.NEF")).unwrap().unwrap().id;
        let metadata = rawtonic_metadata::Metadata {
            title: "Harbour".into(),
            label: "Red".into(),
            rating: 4,
            ..lib.metadata(&id).unwrap()
        };
        lib.set_metadata(&id, &metadata).unwrap();
        let paths = ["A.NEF", "B.NEF"].map(|n| arg(&capture.join(n)));
        let plan = plan_rename(&paths, "{title}{color}{rating}_{seq:auto}");
        assert_eq!(plan.photos[0].to, capture.join("HarbourRed4_1.NEF"));
        assert_eq!(plan.photos[1].to, capture.join("0_2.NEF"));
    }

    #[test]
    fn a_template_with_the_extension_gives_it_once() {
        assert_eq!(
            without_extension("x_DSC.NEF".into(), Path::new("/a/DSC.NEF")),
            "x_DSC"
        );
        assert_eq!(
            without_extension("x.nef".into(), Path::new("/a/DSC.NEF")),
            "x"
        );
        assert_eq!(
            without_extension(".NEF".into(), Path::new("/a/DSC.NEF")),
            ".NEF"
        );
        assert_eq!(
            without_extension("xNEF".into(), Path::new("/a/DSC.NEF")),
            "xNEF"
        );
        assert_eq!(
            without_extension("x.jpg".into(), Path::new("/a/DSC.NEF")),
            "x.jpg"
        );
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &["A.NEF", "A.JPG"]);
        let paths = [arg(&dir.path().join("A.NEF"))];
        let plan = plan_rename(&paths, "Job_{orig}");
        assert_eq!(plan.photos[0].to, dir.path().join("Job_A.NEF"));
    }

    #[test]
    fn a_case_only_rename_is_not_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        write(&dir, &["a.NEF", "a.NEF.rrdata"]);
        let plan = plan_rename(&[arg(&dir.join("a.NEF"))], "A");
        assert!(!plan.blocked(), "{plan:?}");
        assert_eq!(
            renames(&plan),
            ["a.NEF → A.NEF", "a.NEF.rrdata → A.NEF.rrdata"]
        );
    }

    struct NoTrash;
    impl rawtonic_fileops::SystemTrash for NoTrash {
        fn trash(&self, path: &Path) -> anyhow::Result<String> {
            anyhow::bail!("a rename trashes nothing: {}", path.display())
        }
        fn restore(&self, _: &str, _: &Path) -> anyhow::Result<()> {
            anyhow::bail!("nothing to restore")
        }
        fn contains(&self, _: &str) -> bool {
            false
        }
    }

    /// The rename runs exactly the previewed plan: a RAW+JPEG photo with
    /// companion, sidecars and a pair sharing an XMP; the catalog follows
    /// (still paired) and undo restores every name. A plan that changed
    /// since the preview renames nothing.
    #[test]
    fn the_rename_runs_the_previewed_plan_and_undo_restores_every_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, capture) = session(tmp.path());
        let before = [
            "A.NEF",
            "A.JPG",
            "A.xmp",
            "A.NEF.rrdata",
            "A.NEF.a1b2c3.rrdata",
            "A.NEF.jpg",
            "A.NEF.jpg.rrdata",
            "B.NEF",
            "B.JPG",
            "B.xmp",
        ];
        write(&capture, &before);
        let planned = lib
            .plan_add(&capture.join("A.NEF"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        lib.insert_planned(vec![planned]).unwrap();
        lib.add_files(&[
            capture.join("A.NEF.jpg"),
            capture.join("B.NEF"),
            capture.join("B.JPG"),
        ])
        .unwrap();
        let id = lib.asset_at(&capture.join("A.NEF")).unwrap().unwrap().id;
        let paths = ["A.NEF", "B.NEF", "B.JPG"].map(|n| arg(&capture.join(n)));
        let preview = plan_rename(&paths, "Job");
        let expected = preview.files();

        // Changed since the preview: nothing is renamed.
        write(&capture, &["A.NEF.b7c8d9.rrdata"]);
        let error =
            crate::file_management::checked_rename_steps(&paths, "Job", 1, &expected).unwrap_err();
        assert!(error.contains("changed since the preview"), "{error}");
        fs::remove_file(capture.join("A.NEF.b7c8d9.rrdata")).unwrap();

        let (plan, steps, label) =
            crate::file_management::checked_rename_steps(&paths, "Job", 1, &expected)
                .unwrap()
                .unwrap();
        assert_eq!(plan, preview);
        assert_eq!(label, "Rename 3 photos");
        let journal = rawtonic_fileops::Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let op = journal.execute("rename", &label, steps, &NoTrash).unwrap();
        assert_eq!(
            op.renames(),
            expected
                .iter()
                .map(|f| (f.from.clone(), f.to.clone()))
                .collect::<Vec<_>>(),
            "the journal ran the previewed steps"
        );
        crate::file_operations::sync_catalog(&op, true).unwrap();
        assert_eq!(
            names_in(&capture),
            [
                "A.NEF.jpg",
                "A.NEF.jpg.rrdata",
                "Job_1.JPG",
                "Job_1.NEF",
                "Job_1.NEF.a1b2c3.rrdata",
                "Job_1.NEF.rrdata",
                "Job_1.xmp",
                "Job_2.JPG",
                "Job_2.NEF",
                "Job_2.xmp",
            ]
        );
        let asset = lib.asset_at(&capture.join("Job_1.NEF")).unwrap().unwrap();
        assert_eq!(asset.id, id);
        assert_eq!(
            lib.companion_verified(&asset).unwrap(),
            Some(capture.join("Job_1.JPG"))
        );
        assert_eq!(plan.renamed(&paths[2]), arg(&capture.join("Job_2.JPG")));

        let undone = journal.undo(&op.id, &NoTrash).unwrap();
        crate::file_operations::sync_catalog(&undone, false).unwrap();
        let mut sorted = before.map(String::from).to_vec();
        sorted.sort();
        assert_eq!(names_in(&capture), sorted);
        for name in before {
            assert_eq!(fs::read_to_string(capture.join(name)).unwrap(), name);
        }
        let asset = lib.asset_at(&capture.join("A.NEF")).unwrap().unwrap();
        assert_eq!(
            lib.companion_verified(&asset).unwrap(),
            Some(capture.join("A.JPG"))
        );
    }
}

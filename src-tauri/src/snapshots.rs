//! Named snapshots of a photograph's development (G26): Tauri commands over
//! `rawtonic_catalog::snapshots`. A library photograph's (or virtual copy's)
//! snapshots are its library's, and its editor sidecar is derived again
//! after each change; a photograph in no library keeps them in its editor
//! sidecar. Restoring one is the editor's (an ordinary, undoable edit).
use crate::file_management::sidecar_field_edit;
use crate::image_processing::ImageMetadata;
use crate::session_bridge::{copy_for_sidecar, library_dir_for, write_editor_sidecar};
use anyhow::{Context, Result};
use rawtonic_catalog::{Library, Snapshot, snapshots};
use rawtonic_export::sidecars::parse_virtual_path;
use serde_json::Value;
use std::path::Path;
use tauri::Manager;

/// Long side of a snapshot's thumbnail, in pixels.
const THUMBNAIL_SIZE: u32 = 320;

/// A change to the snapshots of one photograph.
enum Change {
    Add { name: String, adjustments: Value },
    Rename { id: String, name: String },
    Update { id: String, adjustments: Value },
    Delete { id: String },
}

/// The snapshots of `path` (a photograph or virtual copy) after `change`
/// (none: as they are). One sidecar edit at a time (`sidecar_field_edit`),
/// so a recipe save that read the sidecar before keeps them.
fn snapshots_after(path: &str, change: Option<Change>) -> Result<Vec<Snapshot>> {
    let (source, sidecar) = parse_virtual_path(path);
    let _turn = sidecar_field_edit();
    match library_dir_for(&source) {
        Some(directory) => in_library(&directory, &source, &sidecar, change),
        None => in_sidecar(&sidecar, change),
    }
}

fn in_library(
    directory: &Path,
    source: &Path,
    sidecar: &Path,
    change: Option<Change>,
) -> Result<Vec<Snapshot>> {
    let lib = Library::open(directory)?;
    let asset = match lib.asset_at(source)? {
        Some(asset) => asset,
        // A photograph of the library's folders not registered yet is
        // registered with what its sidecars hold, as the folder index does.
        None if change.is_some() && source.is_file() => {
            let report = lib.add_files(&[source.to_path_buf()])?;
            if let Some((_, error)) = report.failed.first() {
                anyhow::bail!("{error}");
            }
            lib.asset_at(source)?
                .context("The photograph could not be added to the library")?
        }
        // Not the library's yet: what its sidecar holds.
        None => return Ok(in_file(sidecar)),
    };
    if change.is_some() && source.is_file() && lib.is_untouched(&asset.id)? {
        // A registered photograph whose sidecar was never adopted (it is
        // until the folder is indexed again) adopts it first: the sidecar
        // derived after the change must not replace its edit.
        let contents = rawtonic_catalog::Contents::read_sidecars(source)?;
        lib.adopt_contents(&asset.id, &contents)?;
    }
    let copy = copy_for_sidecar(sidecar).map(|(_, copy)| copy);
    // A virtual copy's sidecar decides its edit while the photograph is
    // there: it is read before anything changes (one that cannot be read
    // stops the change) and keeps all but its snapshots.
    let copy_file: Option<Value> = match (&copy, change.is_some() && source.is_file()) {
        (Some(_), true) if sidecar.is_file() => Some(
            serde_json::from_slice(&std::fs::read(sidecar)?)
                .with_context(|| format!("{} is not a valid editor sidecar", sidecar.display()))?,
        ),
        _ => None,
    };
    let mut version = lib.version_for(&asset.id, copy.as_deref())?;
    if version.is_none() && change.is_some() && sidecar.is_file() {
        // A virtual copy the library has not recorded yet: its sidecar is.
        write_editor_sidecar(sidecar, std::fs::read(sidecar)?)?;
        version = lib.version_for(&asset.id, copy.as_deref())?;
    }
    let Some(version) = version else {
        anyhow::ensure!(change.is_none(), "This virtual copy is not in the library");
        return Ok(in_file(sidecar));
    };
    let Some(change) = change else {
        let kept = lib.snapshots(&version)?;
        // A photograph whose sidecar the library has not adopted yet shows
        // that sidecar's (its first save or snapshot change adopts them).
        if kept.is_empty() && copy.is_none() && source.is_file() && lib.is_untouched(&asset.id)? {
            return Ok(in_file(sidecar));
        }
        return Ok(kept);
    };
    match change {
        Change::Add { name, adjustments } => {
            lib.add_snapshot(&version, &name, adjustments)?;
        }
        Change::Rename { id, name } => lib.rename_snapshot(&version, &id, &name)?,
        Change::Update { id, adjustments } => lib.update_snapshot(&version, &id, adjustments)?,
        Change::Delete { id } => lib.delete_snapshot(&version, &id)?,
    }
    // The sidecar beside the file is derived from the library's state again,
    // which carries the snapshots (offline: the library keeps them only).
    let state = match (&copy, copy_file) {
        (None, _) => lib.editor_state(&asset.id)?,
        (Some(_), Some(mut file)) => {
            snapshots::set_in_state(&mut file, &lib.snapshots(&version)?)?;
            Some(file)
        }
        (Some(copy), None) => lib.copy_state(&asset.id, copy)?,
    };
    if source.is_file()
        && let Some(state) = state
    {
        write_editor_sidecar(sidecar, serde_json::to_vec_pretty(&state)?)?;
    }
    lib.snapshots(&version)
}

/// The snapshots an editor sidecar file holds (none when it cannot be read).
fn in_file(sidecar: &Path) -> Vec<Snapshot> {
    std::fs::read(sidecar)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|state| snapshots::in_state(&state))
        .unwrap_or_default()
}

/// The editor sidecar of a photograph in no library; none there: a new one.
/// One that cannot be read is an error, so nothing writes over it.
fn read_standalone(sidecar: &Path) -> Result<ImageMetadata> {
    match std::fs::read(sidecar) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a valid editor sidecar", sidecar.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ImageMetadata::default()),
        Err(e) => Err(e).with_context(|| format!("Cannot read {}", sidecar.display())),
    }
}

fn in_sidecar(sidecar: &Path, change: Option<Change>) -> Result<Vec<Snapshot>> {
    let mut state = match read_standalone(sidecar) {
        Ok(state) => state,
        // Listed as none; a change says why it cannot be made.
        Err(_) if change.is_none() => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut list = snapshots::in_state(&serde_json::json!({
        snapshots::SIDECAR_KEY: state.snapshots,
        snapshots::STRINGS_KEY: state.snapshot_strings,
    }));
    let Some(change) = change else {
        return Ok(list);
    };
    match change {
        Change::Add { name, adjustments } => {
            snapshots::add_to(&mut list, &name, adjustments)?;
        }
        Change::Rename { id, name } => snapshots::rename_in(&mut list, &id, &name)?,
        Change::Update { id, adjustments } => snapshots::update_in(&mut list, &id, adjustments)?,
        Change::Delete { id } => snapshots::delete_from(&mut list, &id)?,
    }
    (state.snapshots, state.snapshot_strings) = match snapshots::for_sidecar(&list)? {
        Some((Value::Array(list), Value::Object(strings))) => (list, strings),
        _ => Default::default(),
    };
    write_editor_sidecar(sidecar, serde_json::to_vec_pretty(&state)?)?;
    Ok(list)
}

async fn run(path: String, change: Option<Change>) -> Result<Vec<Snapshot>, String> {
    crate::file_management::blocking(move || {
        snapshots_after(&path, change).map_err(|e| format!("{e:#}"))
    })
    .await?
}

#[tauri::command]
pub async fn snapshots_list(path: String) -> Result<Vec<Snapshot>, String> {
    run(path, None).await
}

#[tauri::command]
pub async fn snapshot_create(
    path: String,
    name: String,
    adjustments: Value,
) -> Result<Vec<Snapshot>, String> {
    run(path, Some(Change::Add { name, adjustments })).await
}

#[tauri::command]
pub async fn snapshot_rename(
    path: String,
    id: String,
    name: String,
) -> Result<Vec<Snapshot>, String> {
    run(path, Some(Change::Rename { id, name })).await
}

/// Lightroom's Update with Current Settings: the snapshot `id` holds
/// `adjustments` from now on.
#[tauri::command]
pub async fn snapshot_update(
    path: String,
    id: String,
    adjustments: Value,
) -> Result<Vec<Snapshot>, String> {
    run(path, Some(Change::Update { id, adjustments })).await
}

#[tauri::command]
pub async fn snapshot_delete(path: String, id: String) -> Result<Vec<Snapshot>, String> {
    run(path, Some(Change::Delete { id })).await
}

/// A small JPEG of `path` developed with `adjustments` (a snapshot's) as the
/// Library develops its thumbnails, from the file and for the snapshot's
/// colour noise filter: never from the open photograph's decoded image,
/// which holds the current edit's decode-time development, nor from a base
/// or preview proxy made for other settings. Like the Library's thumbnails
/// it leaves AI Denoise out (at 56 × 40 points it cannot be seen).
#[tauri::command]
pub async fn snapshot_thumbnail(
    path: String,
    adjustments: Value,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<u8>> {
        let state = app_handle.state::<crate::app_state::AppState>();
        let gpu = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
        let (source, _) = parse_virtual_path(&path);
        // Snapshots of one colour noise filter share a decoded base.
        let multiscale = rawtonic_engine::color_noise::multiscale(
            &adjustments,
            crate::formats::is_raw_file(&source),
        );
        let mut adjustments = adjustments;
        let _ =
            rawtonic_export::exif_processing::resolve_unresolved_lens(&mut adjustments, &source);
        let metadata = ImageMetadata {
            adjustments,
            ..Default::default()
        };
        let image = crate::file_management::develop_thumbnail(
            &path,
            &format!("{path}\0snapshot\0{multiscale}"),
            Some(metadata),
            true,
            gpu.as_ref(),
            None,
            &app_handle,
        )
        .context("Developing the snapshot failed")?;
        crate::file_management::encode_thumbnail(&image, THUMBNAIL_SIZE)
    })
    .await
    .map_err(|e| e.to_string())?
    .map(tauri::ipc::Response::new)
    .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif_processing::load_sidecar;
    use rawtonic_catalog::Mode;
    use serde_json::json;
    use std::fs;

    fn add(path: &Path, name: &str, adjustments: Value) -> Vec<Snapshot> {
        snapshots_after(
            &path.to_string_lossy(),
            Some(Change::Add {
                name: name.into(),
                adjustments,
            }),
        )
        .unwrap()
    }

    fn sidecar_snapshots(sidecar: &Path) -> Vec<String> {
        let state: Value = serde_json::from_slice(&fs::read(sidecar).unwrap()).unwrap();
        snapshots::in_state(&state)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    /// The editor's save: what the sidecar holds, with new adjustments.
    fn save_recipe(sidecar: &Path, adjustments: Value) {
        let _turn = sidecar_field_edit();
        let mut state = load_sidecar(sidecar);
        state.adjustments = adjustments;
        write_editor_sidecar(sidecar, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    }

    #[test]
    fn a_photograph_in_no_library_keeps_snapshots_in_its_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let photo = temp.path().join("A.jpg");
        fs::write(&photo, b"plain fixture").unwrap();
        let sidecar = temp.path().join("A.jpg.rrdata");
        // Listing writes nothing.
        assert!(
            snapshots_after(&photo.to_string_lossy(), None)
                .unwrap()
                .is_empty()
        );
        assert!(!sidecar.exists());
        save_recipe(&sidecar, json!({"exposure": 0.5}));
        let before = fs::read(&sidecar).unwrap();
        assert!(!String::from_utf8_lossy(&before).contains("snapshots"));

        let list = add(&photo, "Bright", json!({"exposure": 0.5}));
        add(&photo, "Dark", json!({"exposure": -1}));
        assert_eq!(sidecar_snapshots(&sidecar), ["Bright", "Dark"]);
        // Edits keep them; the edit is the photo's adjustments only.
        save_recipe(&sidecar, json!({"exposure": 2}));
        assert_eq!(sidecar_snapshots(&sidecar), ["Bright", "Dark"]);
        assert_eq!(load_sidecar(&sidecar).adjustments, json!({"exposure": 2}));
        let path = photo.to_string_lossy().into_owned();
        let renamed = snapshots_after(
            &path,
            Some(Change::Rename {
                id: list[0].id.clone(),
                name: "Bright 2".into(),
            }),
        )
        .unwrap();
        assert_eq!(renamed[0].name, "Bright 2");
        assert_eq!(renamed[0].adjustments, json!({"exposure": 0.5}));
        let updated = snapshots_after(
            &path,
            Some(Change::Update {
                id: list[0].id.clone(),
                adjustments: json!({"exposure": 0.7}),
            }),
        )
        .unwrap();
        assert_eq!(updated[0].adjustments, json!({"exposure": 0.7}));
        assert_eq!(updated[0].name, "Bright 2");
        snapshots_after(
            &path,
            Some(Change::Delete {
                id: list[0].id.clone(),
            }),
        )
        .unwrap();
        assert_eq!(sidecar_snapshots(&sidecar), ["Dark"]);
        assert!(
            snapshots_after(
                &path,
                Some(Change::Delete {
                    id: list[0].id.clone()
                })
            )
            .is_err()
        );
        assert_eq!(fs::read(&photo).unwrap(), b"plain fixture");
    }

    #[test]
    fn an_unreadable_sidecar_is_never_written_over() {
        let temp = tempfile::tempdir().unwrap();
        let photo = temp.path().join("A.jpg");
        fs::write(&photo, b"plain fixture").unwrap();
        let sidecar = temp.path().join("A.jpg.rrdata");
        fs::write(&sidecar, b"{\"adjustments\": {\"exposure\": 1.0").unwrap();
        let path = photo.to_string_lossy().into_owned();
        let change = || {
            Some(Change::Add {
                name: "Look".into(),
                adjustments: json!({}),
            })
        };
        assert!(snapshots_after(&path, change()).is_err());
        assert!(snapshots_after(&path, None).unwrap().is_empty());
        assert_eq!(
            fs::read(&sidecar).unwrap(),
            b"{\"adjustments\": {\"exposure\": 1.0"
        );
    }

    #[test]
    fn a_library_photograph_keeps_snapshots_in_the_library_and_its_derived_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Snaps", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        fs::write(&photo, b"library fixture").unwrap();
        let sidecar = lib.directory.join("Capture/A.ARW.rrdata");
        save_recipe(&sidecar, json!({"exposure": 0.5}));

        add(&photo, "Bright", json!({"exposure": 0.5}));
        let asset = lib.asset_at(&photo).unwrap().unwrap();
        let version = lib.primary_version(&asset.id).unwrap();
        assert_eq!(lib.snapshots(&version).unwrap()[0].name, "Bright");
        assert_eq!(sidecar_snapshots(&sidecar), ["Bright"]);
        // A writer with a stale state (read before the snapshot) does not
        // drop it from the sidecar: the library's are written.
        write_editor_sidecar(
            &sidecar,
            br#"{"version":1,"rating":0,"tags":[],"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        assert_eq!(sidecar_snapshots(&sidecar), ["Bright"]);
        assert_eq!(lib.snapshots(&version).unwrap().len(), 1);

        // A virtual copy has its own; a new copy starts without any.
        let copy_sidecar = lib.directory.join("Capture/A.ARW.c0ffee.rrdata");
        write_editor_sidecar(
            &copy_sidecar,
            crate::file_management::without_snapshots(fs::read(&sidecar).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(sidecar_snapshots(&copy_sidecar).is_empty());
        let copy_path = format!("{}?vc=c0ffee", photo.to_string_lossy());
        add(Path::new(&copy_path), "Copy look", json!({"exposure": -2}));
        assert_eq!(sidecar_snapshots(&copy_sidecar), ["Copy look"]);
        assert_eq!(sidecar_snapshots(&sidecar), ["Bright"]);
        let copy = lib.version_for(&asset.id, Some("c0ffee")).unwrap().unwrap();
        assert_eq!(lib.snapshots(&copy).unwrap()[0].name, "Copy look");
        assert_eq!(fs::read(&photo).unwrap(), b"library fixture");
    }

    /// Timing of the snapshot commands on a library photograph with a
    /// realistic recipe (three AI-mask bitmaps of 300 KB, 60 settings),
    /// beside the editor's own recipe save. Catalog and sidecar work only
    /// (no thumbnail). `cargo test -p rawtonic-app --lib snapshot_timing --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn snapshot_timing() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Timing", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        fs::write(&photo, b"library fixture").unwrap();
        let sidecar = lib.directory.join("Capture/A.ARW.rrdata");
        let mut adjustments = serde_json::Map::new();
        for i in 0..60 {
            adjustments.insert(format!("setting{i}"), json!(i as f64 * 0.5));
        }
        let masks: Vec<Value> = (0..3)
            .map(|i| json!({"id": format!("m{i}"), "subMasks": [{"type": "ai-sky",
                "parameters": {"maskDataBase64": format!("data:image/png;base64,{}", char::from(b'a' + i as u8).to_string().repeat(300_000))}}]}))
            .collect();
        adjustments.insert("masks".into(), Value::Array(masks));
        let adjustments = Value::Object(adjustments);
        save_recipe(&sidecar, adjustments.clone());
        let median = |mut times: Vec<f64>| {
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        };
        let time = |f: &mut dyn FnMut()| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        };
        let path = photo.to_string_lossy().into_owned();
        let (mut save, mut create, mut list, mut rename, mut delete) =
            (vec![], vec![], vec![], vec![], vec![]);
        for i in 0..20 {
            let mut edit = adjustments.clone();
            edit["setting0"] = json!(i);
            save.push(time(&mut || save_recipe(&sidecar, edit.clone())));
            create.push(time(&mut || {
                add(&photo, &format!("Look {i}"), edit.clone());
            }));
            list.push(time(&mut || {
                snapshots_after(&path, None).unwrap();
            }));
        }
        // The editor's save with the 20 snapshots held (the sidecar carries them).
        let mut save_held = vec![];
        for i in 0..20 {
            let mut edit = adjustments.clone();
            edit["setting1"] = json!(i);
            save_held.push(time(&mut || save_recipe(&sidecar, edit.clone())));
        }
        let held_kb = fs::metadata(&sidecar).unwrap().len() / 1024;
        let ids: Vec<String> = snapshots_after(&path, None)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        for id in &ids {
            rename.push(time(&mut || {
                snapshots_after(
                    &path,
                    Some(Change::Rename {
                        id: id.clone(),
                        name: "Renamed".into(),
                    }),
                )
                .unwrap();
            }));
        }
        for id in &ids {
            delete.push(time(&mut || {
                snapshots_after(&path, Some(Change::Delete { id: id.clone() })).unwrap();
            }));
        }
        eprintln!(
            "median ms over 20: recipe save {:.1} (with 20 snapshots {:.1}, sidecar {held_kb} KB), snapshot create {:.1}, list (20 held) {:.1}, rename {:.1}, delete {:.1}; sidecar {} KB",
            median(save),
            median(save_held),
            median(create),
            median(list),
            median(rename),
            median(delete),
            fs::metadata(&sidecar).unwrap().len() / 1024
        );
    }

    #[test]
    fn an_unadopted_sidecar_keeps_its_edit_when_its_photograph_is_snapshotted() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Snaps", Mode::Session).unwrap();
        let day = lib.directory.join("Capture");
        let edited =
            br#"{"version":1,"rating":4,"tags":["user:dog"],"adjustments":{"exposure":1.5}}"#;
        // Not registered at all, and registered without its sidecar adopted.
        let (a, b) = (day.join("A.ARW"), day.join("B.ARW"));
        for photo in [&a, &b] {
            fs::write(photo, b"library fixture").unwrap();
            fs::write(
                photo.with_file_name(format!(
                    "{}.rrdata",
                    photo.file_name().unwrap().to_string_lossy()
                )),
                edited,
            )
            .unwrap();
        }
        lib.add_asset(&b).unwrap();
        for photo in [&a, &b] {
            add(photo, "Look", json!({"exposure": 1.5}));
            let sidecar = photo.with_file_name(format!(
                "{}.rrdata",
                photo.file_name().unwrap().to_string_lossy()
            ));
            let state = load_sidecar(&sidecar);
            assert_eq!(
                state.adjustments["exposure"],
                json!(1.5),
                "{}",
                photo.display()
            );
            assert_eq!(state.rating, 4);
            assert_eq!(sidecar_snapshots(&sidecar), ["Look"]);
            let asset = lib.asset_at(photo).unwrap().unwrap();
            assert_eq!(lib.metadata(&asset.id).unwrap().keywords, ["dog"]);
        }
    }

    #[test]
    fn a_virtual_copy_keeps_its_own_sidecar_edit_and_an_unreadable_one_stops_the_change() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Snaps", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        fs::write(&photo, b"library fixture").unwrap();
        let copy_sidecar = lib.directory.join("Capture/A.ARW.c0ffee.rrdata");
        write_editor_sidecar(
            &copy_sidecar,
            br#"{"version":1,"rating":0,"tags":[],"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        // The copy's sidecar changes beside the library (another machine's
        // sync): it decides while the photograph is there.
        fs::write(
            &copy_sidecar,
            br#"{"version":1,"rating":3,"tags":[],"adjustments":{"exposure":2}}"#,
        )
        .unwrap();
        let copy_path = format!("{}?vc=c0ffee", photo.to_string_lossy());
        add(Path::new(&copy_path), "Copy look", json!({"exposure": 2}));
        let state: Value = serde_json::from_slice(&fs::read(&copy_sidecar).unwrap()).unwrap();
        assert_eq!(state["adjustments"]["exposure"], 2);
        assert_eq!(state["rating"], 3);
        assert_eq!(sidecar_snapshots(&copy_sidecar), ["Copy look"]);

        fs::write(&copy_sidecar, b"{ broken").unwrap();
        let id = snapshots_after(&copy_path, None).unwrap()[0].id.clone();
        assert!(snapshots_after(&copy_path, Some(Change::Delete { id })).is_err());
        assert_eq!(fs::read(&copy_sidecar).unwrap(), b"{ broken");
        let asset = lib.asset_at(&photo).unwrap().unwrap();
        let copy = lib.version_for(&asset.id, Some("c0ffee")).unwrap().unwrap();
        assert_eq!(lib.snapshots(&copy).unwrap().len(), 1, "nothing changed");
    }

    #[test]
    fn the_first_save_of_a_photograph_new_to_the_library_keeps_its_sidecar_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Snaps", Mode::Session).unwrap();
        let day = lib.directory.join("Capture");
        let carried = br#"{"version":1,"rating":2,"tags":[],"adjustments":{"exposure":1},
            "snapshots":[{"id":"s1","name":"Warm","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{"temperature":20}}]}"#;
        // Not registered, and registered without its sidecar adopted.
        let (a, b) = (day.join("A.ARW"), day.join("B.ARW"));
        for photo in [&a, &b] {
            fs::write(photo, b"library fixture").unwrap();
            fs::write(
                photo.with_file_name(format!(
                    "{}.rrdata",
                    photo.file_name().unwrap().to_string_lossy()
                )),
                carried,
            )
            .unwrap();
        }
        lib.add_asset(&b).unwrap();
        for photo in [&a, &b] {
            let sidecar = photo.with_file_name(format!(
                "{}.rrdata",
                photo.file_name().unwrap().to_string_lossy()
            ));
            // Listed from the sidecar before the library has them.
            let listed = snapshots_after(&photo.to_string_lossy(), None).unwrap();
            assert_eq!(listed[0].name, "Warm");
            save_recipe(&sidecar, json!({"exposure": 1.5}));
            assert_eq!(sidecar_snapshots(&sidecar), ["Warm"], "{}", photo.display());
            let asset = lib.asset_at(photo).unwrap().unwrap();
            let version = lib.primary_version(&asset.id).unwrap();
            assert_eq!(lib.snapshots(&version).unwrap()[0].name, "Warm");
        }
        // Once the library's, a later state carrying them adds nothing back
        // after they are deleted.
        let sidecar = day.join("A.ARW.rrdata");
        let id = snapshots_after(&a.to_string_lossy(), None).unwrap()[0]
            .id
            .clone();
        snapshots_after(&a.to_string_lossy(), Some(Change::Delete { id })).unwrap();
        write_editor_sidecar(&sidecar, carried).unwrap();
        assert!(sidecar_snapshots(&sidecar).is_empty());
    }

    #[test]
    fn a_never_edited_library_photograph_can_be_snapshotted() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Snaps", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/B.ARW");
        fs::write(&photo, b"library fixture").unwrap();
        add(&photo, "As shot", json!({}));
        let sidecar = lib.directory.join("Capture/B.ARW.rrdata");
        assert_eq!(sidecar_snapshots(&sidecar), ["As shot"]);
        assert!(
            load_sidecar(&sidecar).adjustments.is_null(),
            "still unedited"
        );
    }
}

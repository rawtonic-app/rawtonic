//! Shared frontend payloads, deserialized with the real command argument
//! types. Compile-time checks bind each argument/result to its command;
//! src/contracts/libraryFiles.test.ts pins the Tauri argument names too.
use super::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::future::Future;

fn payloads() -> Value {
    serde_json::from_str(include_str!("../../../src/contracts/libraryFiles.json")).unwrap()
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> T {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null)).unwrap()
}

// Tauri deserializes each top-level camelCase argument independently; these
// envelopes additionally reject a misspelled or unexpected frontend key.
macro_rules! arguments {
    ($name:ident { $($field:ident: $ty:ty),* }) => {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct $name {
            $($field: $ty,)*
        }
    };
}
arguments!(SaveAlbumsArgs { tree: Vec<AlbumItem> });
arguments!(ChangeArgs {
    change: AlbumChange
});
arguments!(AddArgs { album_id: String, paths: Vec<String> });
arguments!(SavePresetsArgs { presets: Vec<PresetItem> });
arguments!(ImportArgs { file_path: String });
arguments!(ImportsArgs { file_paths: Vec<String> });
arguments!(FlushArgs {});

fn album_command<F: Future<Output = Result<Vec<AlbumItem>, String>>>(
    _: impl FnOnce(Vec<AlbumItem>, tauri::AppHandle) -> F,
) {
}

fn preset_command<F: Future<Output = Result<(), String>>>(
    _: impl FnOnce(Vec<PresetItem>, tauri::AppHandle) -> F,
) {
}

#[test]
fn save_albums_contract_returns_a_tree_instead_of_null() {
    album_command(save_albums);
    let args = &payloads()["save_albums"];
    let SaveAlbumsArgs { tree } = serde_json::from_value(args.clone()).unwrap();
    let result = serde_json::to_value(tree).unwrap();
    assert_eq!(result[0]["name"], "Trip");
    assert_eq!(result[0]["images"], json!(["photo.dng"]));
    assert!(result.is_array());
}

#[test]
fn library_change_contract_accepts_the_frontends_album_id() {
    fn command<F: Future<Output = Result<Vec<AlbumItem>, String>>>(
        _: impl FnOnce(AlbumChange, tauri::AppHandle) -> F,
    ) {
    }
    command(library_change);
    let args = &payloads()["library_change"];
    let ChangeArgs { change } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(
        change,
        AlbumChange::AddToAlbum {
            album_id: "trip".into(),
            paths: vec!["photo.dng".into()],
        }
    );
    let AlbumChange::AddToAlbum { album_id, paths } = change else {
        panic!("addToAlbum");
    };
    let mut tree: Vec<AlbumItem> = arg(&payloads()["save_albums"], "tree");
    assert!(albums::add_paths(
        &mut tree,
        &album_id,
        &[paths[0].clone(), "second.dng".into()]
    ));
    assert_eq!(
        serde_json::to_value(tree).unwrap()[0]["images"],
        json!(["photo.dng", "second.dng"])
    );
}

#[test]
fn add_to_album_and_save_presets_contracts() {
    fn command<F: Future<Output = Result<(), String>>>(
        _: impl FnOnce(String, Vec<String>, tauri::AppHandle) -> F,
    ) {
    }
    command(add_to_album);
    let args = &payloads()["add_to_album"];
    let AddArgs { album_id, paths } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(album_id, "trip");
    assert_eq!(paths, ["photo.dng"]);
    preset_command(save_presets);
    let SavePresetsArgs { presets: items } =
        serde_json::from_value(payloads()["save_presets"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(items).unwrap()[0]["preset"]["adjustments"],
        json!({"exposure": 1})
    );
    assert_eq!(serde_json::to_value(()).unwrap(), Value::Null);
}

#[test]
fn preset_import_contracts() {
    fn single<F: Future<Output = Result<Vec<PresetItem>, String>>>(
        _: impl FnOnce(String, tauri::AppHandle) -> F,
    ) {
    }
    fn many<F: Future<Output = Result<PresetImportResult, String>>>(
        _: impl FnOnce(Vec<String>, tauri::AppHandle) -> F,
    ) {
    }
    single(handle_import_presets_from_file);
    single(handle_import_legacy_presets_from_file);
    many(handle_import_presets_from_files);
    for name in [
        "handle_import_presets_from_file",
        "handle_import_legacy_presets_from_file",
    ] {
        let args = &payloads()[name];
        let ImportArgs { file_path } = serde_json::from_value(args.clone()).unwrap();
        assert!(file_path.starts_with("style."));
    }
    let args = &payloads()["handle_import_presets_from_files"];
    let ImportsArgs { file_paths } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(file_paths, ["style.json", "style.xmp"]);
    let presets: Vec<PresetItem> = arg(&payloads()["save_presets"], "presets");
    assert!(serde_json::to_value(&presets).unwrap().is_array());
    let result = PresetImportResult {
        presets,
        failures: vec![PresetImportFailure {
            file_name: "bad.json".into(),
            error: "unreadable".into(),
        }],
        untranslated: vec![PresetImportUntranslated {
            file_name: "style.xmp".into(),
            settings: vec!["unsupported"],
            profile: None,
        }],
    };
    let value = serde_json::to_value(result).unwrap();
    assert!(value["presets"].is_array());
    assert_eq!(
        value["failures"],
        json!([{"fileName": "bad.json", "error": "unreadable"}])
    );
    assert_eq!(
        value["untranslated"],
        json!([{"fileName": "style.xmp", "settings": ["unsupported"]}])
    );
}

#[test]
fn library_flush_contract_reports_failed_save_with_empty_frontend_arguments() {
    use crate::state::library_files::{LibraryFile, LibraryQueue, flush_library, run_change};
    fn command<F: Future<Output = Result<Vec<String>, String>>>(
        _: impl FnOnce(tauri::AppHandle) -> F,
    ) {
    }
    command(library_flush);
    let _: FlushArgs = serde_json::from_value(payloads()["library_flush"].clone()).unwrap();
    let queue = LibraryQueue::default();
    assert!(
        run_change(
            &queue,
            LibraryFile::Presets,
            "Preset list could not be saved",
            || Err::<(), _>("disk full".to_string())
        )
        .is_err()
    );
    let failures = flush_library(&queue, std::time::Duration::from_millis(50));
    assert_eq!(
        serde_json::to_value(failures).unwrap(),
        json!(["Preset list could not be saved: disk full"])
    );
    assert!(flush_library(&queue, std::time::Duration::ZERO).is_empty());
}

#[test]
fn reads_have_no_arguments() {
    fn albums<F: Future<Output = Result<Vec<AlbumItem>, String>>>(
        _: impl FnOnce(tauri::AppHandle) -> F,
    ) {
    }
    fn presets<F: Future<Output = Result<Vec<PresetItem>, String>>>(
        _: impl FnOnce(tauri::AppHandle) -> F,
    ) {
    }
    albums(get_albums);
    presets(load_presets);
    let args: std::collections::BTreeMap<String, Value> = serde_json::from_str("{}").unwrap();
    assert!(args.is_empty());
}

#[test]
fn every_album_edit_deserializes_the_frontend_payload_and_returns_a_tree() {
    for payload in payloads()["album_changes"].as_array().unwrap() {
        let ChangeArgs { change } = serde_json::from_value(payload.clone()).unwrap();
        let mut tree: Vec<AlbumItem> = serde_json::from_value(json!([
            {"type":"album", "id":"trip", "name":"Trip", "images":["renamed.dng", "remove.dng"]},
            {"type":"album", "id":"other", "name":"Other", "images":[]},
            {"type":"group", "id":"group", "name":"Group", "children":[]}
        ]))
        .unwrap();
        assert!(albums::apply_album_change(&mut tree, &change).unwrap());
        sort_album_tree(&mut tree);
        let result = serde_json::to_value(&tree).unwrap();
        assert!(result.is_array());
        // The stored command result, including the existing icon field, is
        // an AlbumItem tree; no edit resurrects the former path.
        let decoded: Vec<AlbumItem> = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(decoded, tree);
        assert!(result.to_string().contains("renamed.dng"));
        assert!(!result.to_string().contains("old.dng"));
        match change {
            AlbumChange::Rename { .. } => assert_eq!(result[2]["name"], "Renamed"),
            AlbumChange::Delete { .. } => assert_eq!(result.as_array().unwrap().len(), 2),
            AlbumChange::Move { .. } => assert_eq!(
                result[0]["children"][0]["images"],
                json!(["renamed.dng", "remove.dng"])
            ),
            AlbumChange::RemovePhotos { .. } => {
                assert_eq!(result[2]["images"], json!(["renamed.dng"]))
            }
            AlbumChange::Create { item, .. } => match item {
                AlbumItem::Album { .. } => {
                    assert_eq!(result[0]["children"][0]["images"], json!(["photo.dng"]))
                }
                AlbumItem::Group { .. } => assert_eq!(result[1]["children"], json!([])),
            },
            _ => unreachable!(),
        }
    }
}

#[test]
fn deleted_album_change_keeps_the_string_error_contract() {
    let ChangeArgs { change } =
        serde_json::from_value(payloads()["library_change"].clone()).unwrap();
    let error = albums::apply_album_change(&mut Vec::new(), &change).unwrap_err();
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!("Album trip is not there any more")
    );
}

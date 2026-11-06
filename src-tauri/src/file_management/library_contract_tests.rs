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
            ticket: Option<u64>,
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
    _: impl FnOnce(Vec<AlbumItem>, Option<u64>, tauri::AppHandle) -> F,
) {
}

fn preset_command<F: Future<Output = Result<(), String>>>(
    _: impl FnOnce(Vec<PresetItem>, Option<u64>, tauri::AppHandle) -> F,
) {
}

#[test]
fn save_albums_contract_returns_a_tree_instead_of_null() {
    album_command(save_albums);
    let args = &payloads()["save_albums"];
    let SaveAlbumsArgs { tree, ticket } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(ticket, None);
    let result = serde_json::to_value(tree).unwrap();
    assert_eq!(result[0]["name"], "Trip");
    assert_eq!(result[0]["images"], json!(["photo.dng"]));
    assert!(result.is_array());
}

#[test]
fn library_change_contract_accepts_the_frontends_album_id() {
    fn command<F: Future<Output = Result<Vec<AlbumItem>, String>>>(
        _: impl FnOnce(AlbumChange, Option<u64>, tauri::AppHandle) -> F,
    ) {
    }
    command(library_change);
    let args = &payloads()["library_change"];
    let ChangeArgs { change, ticket } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(ticket, None);
    assert_eq!(
        change,
        AlbumChange::AddToAlbum {
            album_id: "trip".into(),
            paths: vec!["photo.dng".into()],
        }
    );
    let AlbumChange::AddToAlbum { album_id, paths } = change;
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
        _: impl FnOnce(String, Vec<String>, Option<u64>, tauri::AppHandle) -> F,
    ) {
    }
    command(add_to_album);
    let args = &payloads()["add_to_album"];
    let AddArgs {
        album_id,
        paths,
        ticket,
    } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(album_id, "trip");
    assert_eq!(paths, ["photo.dng"]);
    assert_eq!(ticket, None);
    preset_command(save_presets);
    let SavePresetsArgs {
        presets: items,
        ticket,
    } = serde_json::from_value(payloads()["save_presets"].clone()).unwrap();
    assert_eq!(ticket, None);
    assert_eq!(
        serde_json::to_value(items).unwrap()[0]["preset"]["adjustments"],
        json!({"exposure": 1})
    );
    assert_eq!(serde_json::to_value(()).unwrap(), Value::Null);
}

#[test]
fn preset_import_contracts() {
    fn single<F: Future<Output = Result<Vec<PresetItem>, String>>>(
        _: impl FnOnce(String, Option<u64>, tauri::AppHandle) -> F,
    ) {
    }
    fn many<F: Future<Output = Result<PresetImportResult, String>>>(
        _: impl FnOnce(Vec<String>, Option<u64>, tauri::AppHandle) -> F,
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
        let ImportArgs { file_path, ticket } = serde_json::from_value(args.clone()).unwrap();
        assert!(file_path.starts_with("style."));
        assert_eq!(ticket, None);
    }
    let args = &payloads()["handle_import_presets_from_files"];
    let ImportsArgs { file_paths, ticket } = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(file_paths, ["style.json", "style.xmp"]);
    assert_eq!(ticket, None);
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
    use crate::state::library_files::{LibraryQueue, flush_library, next_after_sent, run_change};
    fn command<F: Future<Output = Result<Vec<String>, String>>>(
        _: impl FnOnce(Option<u64>, tauri::AppHandle) -> F,
    ) {
    }
    command(library_flush);
    let args = &payloads()["library_flush"];
    let FlushArgs { ticket } = serde_json::from_value(args.clone()).unwrap();
    let queue = LibraryQueue::default();
    assert!(
        run_change(
            &queue,
            next_after_sent(&queue),
            "Preset list could not be saved",
            || Err::<(), _>("disk full".to_string())
        )
        .is_err()
    );
    let failures = flush_library(&queue, ticket, std::time::Duration::from_millis(50), Ok);
    assert_eq!(
        serde_json::to_value(failures).unwrap(),
        json!(["Preset list could not be saved: disk full"])
    );
    assert!(flush_library(&queue, ticket, std::time::Duration::from_millis(50), Ok).is_empty());
}

#[test]
fn reads_and_ticket_allocator_contracts() {
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
    let _: fn(tauri::AppHandle) -> u64 = queue_ticket;
    let args: std::collections::BTreeMap<String, Value> = serde_json::from_str("{}").unwrap();
    assert!(args.is_empty());
    let queue = crate::state::library_files::LibraryQueue::default();
    let ticket = crate::state::library_files::next_after_sent(&queue);
    assert_eq!(serde_json::to_value(ticket).unwrap(), json!(1));
}

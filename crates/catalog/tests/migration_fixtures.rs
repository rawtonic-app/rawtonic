//! Historical catalogs, one per schema version, upgraded by the library.
//!
//! `fixtures/migrations/vNN.sql` is a populated database as schema NN left
//! it (synthetic photographs only: RAW + JPEG pairs, virtual copies, stacks,
//! albums, smart albums, snapshots, keywords, capture facts, sidecar state,
//! recipes with history, …). `vNN.expected` is what opening it gave when the
//! fixture was recorded: the schema, every table's rows (sorted), the
//! foreign-key check and a fixed set of library searches. Every fixture must
//! upgrade to exactly that, so a change to the migrations or their runner
//! that moves, loses or changes data fails here.
//!
//! Regenerating: `RAWTONIC_WRITE_MIGRATION_FIXTURES=1 cargo test -p
//! rawtonic-catalog --test migration_fixtures -- --ignored` writes the
//! `.sql` fixtures (from the migration scripts themselves, filled by
//! `fill`); `RAWTONIC_BLESS_MIGRATIONS=1` records the `.expected` files with
//! the code under test. Neither is done for a refactor: the recorded
//! expectations are what the code must keep producing.
use rawtonic_catalog::{DB_SCHEMA_VERSION, Library, Mode, Rule};
use rusqlite::{Connection, types::Value as Sql};
use serde_json::json;
use std::{fmt::Write as _, fs, path::Path, path::PathBuf};

/// Migration script `version` as the library ships it.
fn script(version: usize) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("migrations/{version:04}.sql"));
    fs::read_to_string(path).unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations")
}

/// The SQL functions the migration scripts and triggers call, as the
/// library registers them (`Library::configure`).
fn register_functions(conn: &Connection) {
    use rusqlite::functions::FunctionFlags as F;
    let flags = F::SQLITE_UTF8 | F::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("rawtonic_fold", 1, flags, |ctx| {
        Ok(match ctx.get_raw(0) {
            rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8_lossy(t).to_lowercase()),
            rusqlite::types::ValueRef::Integer(n) => Some(n.to_string()),
            rusqlite::types::ValueRef::Real(n) => Some(n.to_string()),
            _ => None,
        })
    })
    .unwrap();
    conn.create_scalar_function("rawtonic_trim", 1, flags, |ctx| {
        Ok(match ctx.get_raw(0) {
            rusqlite::types::ValueRef::Text(t) => {
                Some(String::from_utf8_lossy(t).trim().to_string())
            }
            _ => None,
        })
    })
    .unwrap();
    conn.create_scalar_function("rawtonic_ai_facts", 1, flags, |ctx| {
        Ok(match ctx.get_raw(0) {
            rusqlite::types::ValueRef::Text(t) => i64::from(
                rawtonic_metadata::provenance::ai_facts_of_text(&String::from_utf8_lossy(t)),
            ),
            _ => 0,
        })
    })
    .unwrap();
}

/// A database exactly as schema `version` created it: the scripts up to it
/// (schema 22's and 23's columns were part of their scripts then).
fn schema_at(version: usize) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    register_functions(&conn);
    for step in 1..=version {
        match step {
            22 => conn
                .execute_batch(
                    "ALTER TABLE assets ADD COLUMN exposure_mode INTEGER;
                     ALTER TABLE assets ADD COLUMN exposure_bias REAL;
                     ALTER TABLE assets ADD COLUMN sequence INTEGER;",
                )
                .unwrap(),
            23 => conn
                .execute_batch("ALTER TABLE metadata ADD COLUMN xmp_publishing TEXT;")
                .unwrap(),
            _ => {}
        }
        conn.execute_batch(&script(step)).unwrap();
    }
    conn
}

fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?",
        [table],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Inserts a row with the columns this schema has (later columns are left
/// out, as an older Rawtonic would have written the row); no table, no row.
fn put(conn: &Connection, table: &str, row: &[(&str, Sql)]) {
    if !has_table(conn, table) {
        return;
    }
    let present = columns(conn, table);
    let row: Vec<&(&str, Sql)> = row
        .iter()
        .filter(|(c, _)| present.iter().any(|p| p == c))
        .collect();
    let names: Vec<&str> = row.iter().map(|(c, _)| *c).collect();
    let marks = vec!["?"; row.len()].join(",");
    conn.execute(
        &format!("INSERT INTO {table}({}) VALUES({marks})", names.join(",")),
        rusqlite::params_from_iter(row.iter().map(|(_, v)| v)),
    )
    .unwrap_or_else(|e| panic!("{table}: {e}"));
}

fn t(s: &str) -> Sql {
    Sql::Text(s.into())
}
fn i(n: i64) -> Sql {
    Sql::Integer(n)
}
fn r(n: f64) -> Sql {
    Sql::Real(n)
}
const NULL: Sql = Sql::Null;

fn asset_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
fn version_id(n: u32) -> String {
    format!("00000000-0000-4000-9000-{n:012}")
}

const BLOB_HASH: &str = "5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e";
const ALBUM: &str = "album-wedding";
const GROUP: &str = "group-clients";
const NESTED: &str = "album-portraits";
const SMART_ALBUM: &str = "smart-five-stars";

struct Photo {
    n: u32,
    root: &'static str,
    rel: &'static str,
    kind: &'static str,
    pair: Option<&'static str>,
    make: &'static str,
    camera: &'static str,
    lens: &'static str,
    captured: Option<&'static str>,
    iso: Option<i64>,
    rating: i64,
    flag: i64,
    label: &'static str,
    keywords: serde_json::Value,
    xmp: (&'static str, Option<&'static str>),
    missing: i64,
    trashed: i64,
}

fn photos() -> Vec<Photo> {
    let p = |n, rel, kind| Photo {
        n,
        root: "session",
        rel,
        kind,
        pair: None,
        make: "",
        camera: "",
        lens: "",
        captured: None,
        iso: None,
        rating: 0,
        flag: 0,
        label: "",
        keywords: json!(null),
        xmp: ("pending", None),
        missing: 0,
        trashed: 0,
    };
    vec![
        Photo {
            pair: Some("DSC00123.JPG"),
            make: "SONY",
            camera: "ILCE-7RM4",
            lens: "FE 24-70mm F2.8 GM",
            captured: Some("2026-06-01 10:00:00"),
            iso: Some(100),
            rating: 5,
            flag: 1,
            label: "red",
            keywords: json!(["wedding", "ceremony"]),
            xmp: ("synced", Some("digest-1")),
            ..p(1, "Capture/Day 1/DSC00123.ARW", "arw")
        },
        Photo {
            pair: Some("DSC00124.JPG"),
            make: "SONY",
            camera: "ILCE-7RM4",
            lens: "FE 85mm F1.4 GM",
            captured: Some("2026-06-01 18:30:00"),
            iso: Some(3200),
            rating: 4,
            label: "yellow",
            keywords: json!(["wedding", "  sunset ", 7]),
            xmp: ("pending", Some("digest-2")),
            ..p(2, "Capture/Day 1/DSC00124.ARW", "arw")
        },
        Photo {
            make: "Canon",
            camera: "Canon EOS R5",
            lens: "RF 50mm F1.2 L USM",
            iso: Some(800),
            rating: 3,
            flag: -1,
            label: "green",
            keywords: json!(["portrait"]),
            ..p(3, "Capture/Day 2/IMG_0001.CR3", "cr3")
        },
        Photo {
            keywords: json!(["archive", "Wedding"]),
            rating: 1,
            ..p(4, "Capture/scan.jpg", "jpg")
        },
        Photo {
            make: "SONY",
            camera: "ILCE-7RM4",
            captured: Some("2026-06-02 09:15:00"),
            ..p(5, "Output/merge.dng", "dng")
        },
        Photo {
            root: "ext-archive",
            make: "NIKON CORPORATION",
            camera: "NIKON Z 8",
            lens: "NIKKOR Z 24-120mm f/4 S",
            captured: Some("2024-03-10 12:00:00"),
            iso: Some(64),
            rating: 2,
            missing: 1,
            xmp: ("synced", Some("digest-6")),
            ..p(6, "2024/03/DSC_0042.NEF", "nef")
        },
        Photo {
            make: "SONY",
            camera: "ILCE-7RM4",
            captured: Some("2026-06-01 11:00:00"),
            trashed: 1,
            missing: 1,
            ..p(7, "Trash/DSC00099.ARW", "arw")
        },
        // Nothing rated, tagged or edited: schema 18 marks it synced.
        p(8, "Selects/DSC00200.ARW", "arw"),
        Photo {
            make: "FUJIFILM",
            camera: "X-T5",
            captured: Some("2026-06-03 07:45:00"),
            iso: Some(160),
            keywords: json!(["landscape"]),
            ..p(9, "Capture/Day 2/DSCF1234.RAF", "raf")
        },
    ]
}

/// Fills a database of schema `version` the way that version's library
/// wrote it.
fn fill(conn: &Connection, version: usize) {
    put(
        conn,
        "roots",
        &[("id", t("session")), ("path", t(".")), ("external", i(0))],
    );
    put(
        conn,
        "roots",
        &[
            ("id", t("ext-archive")),
            ("path", t("/Volumes/ARCHIVE/Photos")),
            ("volume_id", t("4C1A2B3D-0000-4000-8000-00000000A001")),
            ("external", i(1)),
            ("volume_name", t("ARCHIVE")),
            ("volume_path", t("/Volumes/ARCHIVE")),
            ("volume_kind", t("removable")),
        ],
    );
    for p in photos() {
        let id = asset_id(p.n);
        let search_text = match &p.keywords {
            serde_json::Value::Array(k) => k
                .iter()
                .filter_map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        let companion = |v: Sql| if p.pair.is_some() { v } else { NULL };
        put(
            conn,
            "assets",
            &[
                ("id", t(&id)),
                ("root_id", t(p.root)),
                ("rel_path", t(p.rel)),
                ("size", i(20_000_000 + i64::from(p.n))),
                ("mtime", i(1_780_000_000 + i64::from(p.n))),
                ("fingerprint", t(&format!("xxh3:{:016x}", p.n * 7919))),
                ("capture_time", p.captured.map_or(NULL, t)),
                ("camera", t(p.camera)),
                ("lens", t(p.lens)),
                ("width", i(6000)),
                ("height", i(4000)),
                ("orientation", i(1)),
                ("kind", t(p.kind)),
                ("raw_jpeg_pair", p.pair.map_or(NULL, t)),
                ("missing", i(p.missing)),
                ("search_text", t(&search_text)),
                ("sha256", t(&format!("{:064x}", p.n))),
                ("companion_size", companion(i(5_000_000))),
                (
                    "companion_fingerprint",
                    companion(t("xxh3:00000000000c0001")),
                ),
                (
                    "companion_sha256",
                    companion(t(&format!("{:064x}", 1000 + p.n))),
                ),
                ("make", t(p.make)),
                (
                    "focal_length",
                    if p.lens.is_empty() { NULL } else { r(50.0) },
                ),
                ("aperture", if p.lens.is_empty() { NULL } else { r(2.8) }),
                ("iso", p.iso.map_or(NULL, i)),
                (
                    "exposure",
                    if p.iso.is_some() {
                        r(1.0 / 250.0)
                    } else {
                        NULL
                    },
                ),
                // Every photograph was read once (later schemas read some again).
                ("facts_read", i(1)),
                ("trashed", i(p.trashed)),
                (
                    "source_type",
                    t(if p.kind == "jpg" {
                        "trainedAlgorithmicMedia"
                    } else {
                        ""
                    }),
                ),
                ("exposure_mode", if p.n == 2 { i(2) } else { NULL }),
                ("exposure_bias", if p.n == 2 { r(-0.7) } else { NULL }),
                ("sequence", if p.n == 9 { i(1) } else { NULL }),
            ],
        );
        let mut data = json!({"rating": p.rating, "label": p.label, "flag": p.flag});
        if !p.keywords.is_null() {
            data["keywords"] = p.keywords.clone();
        }
        if p.n == 1 {
            data["title"] = json!("First kiss");
            data["creator"] = json!("Synthetic Photographer");
        }
        let data = if p.n == 8 {
            "{}".to_string()
        } else {
            data.to_string()
        };
        put(
            conn,
            "metadata",
            &[
                ("asset_id", t(&id)),
                ("rating", i(p.rating)),
                ("label", t(p.label)),
                ("flag", i(p.flag)),
                ("data", t(&data)),
                ("xmp_hash", p.xmp.1.map_or(NULL, t)),
                ("xmp_status", t(p.xmp.0)),
                ("from_camera", i(i64::from(p.n == 3))),
                (
                    "xmp_publishing",
                    if p.n == 2 { t("digest-2b") } else { NULL },
                ),
            ],
        );
        put(
            conn,
            "versions",
            &[
                ("id", t(&version_id(p.n))),
                ("asset_id", t(&id)),
                ("name", t("Original")),
                ("is_primary", i(1)),
            ],
        );
    }

    // Recipes with history: photograph 1 has three steps (the last with AI
    // Denoise and a blob), photograph 2 one, photograph 3 two.
    let blob_ref = format!("rawtonic-blob:sha256:{BLOB_HASH}");
    if version >= 7 {
        put(
            conn,
            "recipe_blobs",
            &[
                ("hash", t(BLOB_HASH)),
                ("data", t("data:image/png;base64,iVBORw0KGgo=")),
            ],
        );
    }
    let recipes: [(i64, u32, Option<i64>, String); 6] = [
        (1, 1, None, json!({"adjustments": {"exposure": 0.3}}).to_string()),
        (2, 1, Some(1), json!({"adjustments": {"exposure": 0.5, "contrast": 10}}).to_string()),
        (
            3,
            1,
            Some(2),
            json!({"adjustments": {"exposure": 0.5, "contrast": 10, "aiDenoise": 40, "aiDenoiseVersion": 1,
                "masks": [{"id": "m1", "image": blob_ref}]}})
            .to_string(),
        ),
        (4, 2, None, json!({"adjustments": {"temperature": 5200}}).to_string()),
        (5, 3, None, json!({"adjustments": {"exposure": -0.2}}).to_string()),
        (6, 3, Some(5), json!({"adjustments": {"exposure": -0.2, "vibrance": 15}}).to_string()),
    ];
    for (id, photo, parent, payload) in &recipes {
        put(
            conn,
            "recipes",
            &[
                ("id", i(*id)),
                ("version_id", t(&version_id(*photo))),
                ("parent", parent.map_or(NULL, i)),
                ("schema_version", i(1)),
                ("payload", t(payload)),
                ("snapshot", if *id == 2 { t("Before AI") } else { NULL }),
                ("created_at", t(&format!("2026-06-0{photo} 12:00:0{id}"))),
            ],
        );
    }
    if version >= 7 {
        put(
            conn,
            "recipe_blob_refs",
            &[("recipe_id", i(3)), ("hash", t(BLOB_HASH))],
        );
    }
    for (photo, head) in [(1, 3), (2, 4), (3, 6)] {
        conn.execute(
            "UPDATE versions SET head=? WHERE id=?",
            rusqlite::params![head, version_id(photo)],
        )
        .unwrap();
    }

    // Virtual copies: two of photograph 1 (one with a generated area), one
    // of photograph 2.
    let copies = [
        (101, 1, "a1b2c3", json!({"adjustments": {"exposure": 1.0}})),
        (
            102,
            1,
            "0f0f0f",
            json!({"adjustments": {"aiPatches": [{"id": "p1", "visible": true,
                "patchData": {"model": "external", "color": "rawtonic-blob:sha256:none"}}]}}),
        ),
        (
            103,
            2,
            "beef01",
            json!({"adjustments": {"saturation": -100}}),
        ),
    ];
    for (n, photo, name, state) in &copies {
        put(
            conn,
            "versions",
            &[
                ("id", t(&version_id(*n))),
                ("asset_id", t(&asset_id(*photo))),
                ("name", t(name)),
                ("is_primary", i(0)),
                ("legacy_state", t(&state.to_string())),
            ],
        );
    }
    put(
        conn,
        "masks",
        &[
            ("id", t("mask-1")),
            ("version_id", t(&version_id(1))),
            ("vector_json", t("{\"type\":\"radial\"}")),
            ("raster_path", t("masks/mask-1.png")),
            ("hash", t("mask-hash-1")),
        ],
    );

    // Albums: a group with an album inside, a top-level album, a smart album.
    put(
        conn,
        "collections",
        &[
            ("id", t(GROUP)),
            ("name", t("Clients")),
            ("kind", t("group")),
            ("position", i(0)),
        ],
    );
    put(
        conn,
        "collections",
        &[
            ("id", t(ALBUM)),
            ("name", t("Wedding")),
            ("parent", t(GROUP)),
            ("kind", t("album")),
            ("position", i(0)),
        ],
    );
    put(
        conn,
        "collections",
        &[
            ("id", t(NESTED)),
            ("name", t("Portraits")),
            ("kind", t("album")),
            ("position", i(1)),
        ],
    );
    let smart = json!({"combine": "all", "rules": [{"field": "rating", "op": "ge", "value": 4}]});
    if version >= 11 {
        put(
            conn,
            "collections",
            &[
                ("id", t(SMART_ALBUM)),
                ("name", t("Four stars and up")),
                ("kind", t("smart")),
                ("position", i(2)),
                ("rules", t(&smart.to_string())),
            ],
        );
    } else {
        put(
            conn,
            "smart_collections",
            &[
                ("id", t(SMART_ALBUM)),
                ("name", t("Four stars and up")),
                ("query_json", t(&smart.to_string())),
            ],
        );
    }
    for (album, photo) in [(ALBUM, 1), (ALBUM, 2), (ALBUM, 4), (NESTED, 3), (NESTED, 7)] {
        put(
            conn,
            "collection_assets",
            &[
                ("collection_id", t(album)),
                ("asset_id", t(&asset_id(photo))),
            ],
        );
    }

    // Settings tables of the first schema.
    for (table, column) in [
        ("presets", "recipe_json"),
        ("styles", "recipe_json"),
        ("export_recipes", "recipe_json"),
    ] {
        put(
            conn,
            table,
            &[
                ("id", t(&format!("{table}-1"))),
                ("name", t("Warm")),
                (column, t("{\"temperature\":5600}")),
            ],
        );
    }
    put(
        conn,
        "naming_templates",
        &[
            ("id", t("naming-1")),
            ("name", t("Date and counter")),
            ("template", t("{date}_{counter:4}")),
        ],
    );
    put(
        conn,
        "workspaces",
        &[
            ("id", t("ws-1")),
            ("name", t("Culling")),
            ("settings_json", t("{}")),
        ],
    );
    put(
        conn,
        "keymaps",
        &[
            ("id", t("km-1")),
            ("name", t("Default")),
            ("bindings_json", t("{}")),
        ],
    );
    put(
        conn,
        "tasks",
        &[
            ("id", t("job-1")),
            ("kind", t("ingest")),
            ("state", t("interrupted")),
            ("payload", t("{\"destination\":\"Capture\"}")),
            ("progress", r(0.5)),
            ("created_at", t("2026-06-01 09:00:00")),
        ],
    );
    put(
        conn,
        "ingest_items",
        &[
            ("job_id", t("job-1")),
            ("position", i(0)),
            ("number", i(1)),
            ("source", t("/Volumes/CARD/DCIM/100MSDCF/DSC00123.ARW")),
            ("source_size", i(20_000_001)),
            ("source_mtime_ns", i(1_780_000_000_000_000_001)),
            ("source_fingerprint", t("xxh3:0000000000001eef")),
            ("companion", t("/Volumes/CARD/DCIM/100MSDCF/DSC00123.JPG")),
            ("state", t("done")),
            ("destination", t("Capture/Day 1/DSC00123.ARW")),
        ],
    );
    put(
        conn,
        "file_ops_log",
        &[
            ("id", t("op-1")),
            ("kind", t("rename")),
            ("state", t("committed")),
            ("payload", t("{}")),
            ("created_at", t("2026-06-01 09:30:00")),
        ],
    );

    // Import batches (schema 4): later kinds only where the CHECK allows them.
    let mut batches = vec![(1, "ingest"), (2, "add")];
    if version >= 6 {
        batches.push((3, "hotfolder"));
    }
    if version >= 9 {
        batches.push((4, "tether"));
    }
    for (id, kind) in &batches {
        put(
            conn,
            "import_batches",
            &[
                ("id", i(*id)),
                ("kind", t(kind)),
                ("created_at", t(&format!("2026-06-0{id} 08:00:00"))),
            ],
        );
    }
    for (photo, batch) in [(1, 1), (2, 1), (3, 2), (5, 2), (8, 3), (9, 4)] {
        if batches.iter().any(|(id, _)| *id == batch) {
            put(
                conn,
                "asset_imports",
                &[("asset_id", t(&asset_id(photo))), ("batch_id", i(batch))],
            );
        }
    }

    put(
        conn,
        "hot_folder",
        &[
            ("id", i(1)),
            ("folder", t("/Users/photographer/Tethered")),
            ("template", t("{original}")),
            ("include_subfolders", i(1)),
            ("settings_json", t("{\"rating\":0}")),
            ("last_capture", t(&asset_id(8))),
            ("started_at", t("2026-06-03 08:00:00")),
        ],
    );
    put(
        conn,
        "hot_folder_files",
        &[
            ("rel_path", t("DSC00200.ARW")),
            ("size", i(20_000_008)),
            ("mtime_ns", i(1_780_000_008_000_000_000)),
        ],
    );

    // Snapshots (schema 12), one referring to the blob.
    put(
        conn,
        "snapshots",
        &[
            ("key", i(1)),
            ("id", t("snap-1")),
            ("version_id", t(&version_id(1))),
            ("name", t("Before retouch")),
            ("created_at", t("2026-06-01T12:00:00Z")),
            (
                "adjustments",
                t(&json!({"exposure": 0.3, "mask": blob_ref}).to_string()),
            ),
        ],
    );
    put(
        conn,
        "snapshots",
        &[
            ("key", i(2)),
            ("id", t("snap-2")),
            ("version_id", t(&version_id(101))),
            ("name", t("Copy look")),
            ("created_at", t("2026-06-01T12:05:00Z")),
            ("adjustments", t("{\"exposure\":1.0}")),
        ],
    );
    put(
        conn,
        "snapshot_blob_refs",
        &[("snapshot_key", i(1)), ("hash", t(BLOB_HASH))],
    );

    put(
        conn,
        "folder_root_volumes",
        &[
            ("path", t("/Volumes/ARCHIVE/Photos")),
            ("volume", t("4C1A2B3D-0000-4000-8000-00000000A001")),
        ],
    );
    put(
        conn,
        "capture_overrides",
        &[
            ("asset_id", t(&asset_id(9))),
            ("captured", t("2026-06-03 08:45:00")),
            ("subsec", t("12")),
            ("offset", t("+02:00")),
        ],
    );
    for photo in [1, 5] {
        put(
            conn,
            "image_hashes",
            &[
                ("asset_id", t(&asset_id(photo))),
                ("hash", t("same-image-data")),
                ("size", i(20_000_000 + i64::from(photo))),
                ("mtime", i(1_780_000_000 + i64::from(photo))),
            ],
        );
    }
    put(
        conn,
        "eye_reads",
        &[
            ("asset_id", t(&asset_id(3))),
            ("version", i(2)),
            ("fingerprint", t("xxh3:0000000000005cf3")),
        ],
    );
    put(
        conn,
        "eye_reads",
        &[
            ("asset_id", t(&asset_id(9))),
            ("version", i(2)),
            ("fingerprint", t("xxh3:000000000001166b")),
            ("error", t("unreadable")),
        ],
    );
    for (face, state, over) in [(0, "closed", NULL), (1, "uncertain", t("open"))] {
        put(
            conn,
            "face_eyes",
            &[
                ("asset_id", t(&asset_id(3))),
                ("face", i(face)),
                ("x", r(0.1 + 0.4 * face as f64)),
                ("y", r(0.2)),
                ("w", r(0.2)),
                ("h", r(0.3)),
                ("state", t(state)),
                ("ear", r(0.18)),
                ("accessories", r(0.05)),
                ("override_state", over),
            ],
        );
    }
    // Content search (schema 25): photograph 3 read, photograph 9 not
    // readable.
    put(
        conn,
        "content_embeddings",
        &[
            ("asset_id", t(&asset_id(3))),
            ("model", t("d6e4f91fd3779a52")),
            ("fingerprint", t("xxh3:0000000000005cf3")),
            ("scale", r(0.0625)),
            (
                "vector",
                Sql::Blob((0..768).map(|n| (n % 7) as u8).collect()),
            ),
        ],
    );
    put(
        conn,
        "content_embeddings",
        &[
            ("asset_id", t(&asset_id(9))),
            ("model", t("d6e4f91fd3779a52")),
            ("fingerprint", t("xxh3:000000000001166b")),
            ("error", t("unreadable")),
        ],
    );
    // A stack of photograph 1, its copy and photograph 2 (schema 22).
    put(conn, "stacks", &[("id", t("stack-1")), ("collapsed", i(1))]);
    for (photo, copy, position) in [(1, "", 0), (1, "a1b2c3", 1), (2, "", 2)] {
        put(
            conn,
            "stack_members",
            &[
                ("stack_id", t("stack-1")),
                ("asset_id", t(&asset_id(photo))),
                ("copy", t(copy)),
                ("position", i(position)),
            ],
        );
    }
}

fn render(value: &Sql) -> String {
    match value {
        Sql::Null => "NULL".into(),
        Sql::Integer(n) => n.to_string(),
        Sql::Real(n) => format!("{n:?}"),
        Sql::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Sql::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        ),
    }
}

fn rows(conn: &Connection, sql: &str) -> Vec<Vec<Sql>> {
    let mut statement = conn.prepare(sql).unwrap();
    let count = statement.column_count();
    statement
        .query_map([], |row| (0..count).map(|c| row.get::<_, Sql>(c)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The FTS5 index's own tables, which follow from its contents.
fn shadow(name: &str) -> bool {
    name.starts_with("asset_search_")
}

/// Tables with data (not the index's own, not virtual).
fn data_tables(conn: &Connection) -> Vec<(String, String)> {
    rows(
        conn,
        "SELECT name, sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )
    .into_iter()
    .map(|row| match (&row[0], &row[1]) {
        (Sql::Text(name), Sql::Text(sql)) => (name.clone(), sql.clone()),
        _ => unreachable!(),
    })
    .filter(|(name, _)| !shadow(name))
    .collect()
}

/// A loadable SQL text of the whole database: tables, their rows (the text
/// index's contents included), then indexes and triggers, so loading it
/// fires no trigger.
fn dump(conn: &Connection, version: usize) -> String {
    let mut out = format!(
        "-- A schema {version} library with synthetic photographs (tests/migration_fixtures.rs).\n"
    );
    let tables = data_tables(conn);
    for (_, sql) in &tables {
        writeln!(out, "{sql};").unwrap();
    }
    for (name, sql) in &tables {
        let virtual_table = sql.starts_with("CREATE VIRTUAL TABLE");
        let select = if virtual_table {
            format!("SELECT rowid, * FROM {name} ORDER BY rowid")
        } else if sql.contains("WITHOUT ROWID") {
            format!("SELECT * FROM {name} ORDER BY 1, 2")
        } else {
            format!("SELECT * FROM {name} ORDER BY rowid")
        };
        let mut names = columns(conn, name);
        if virtual_table {
            names.insert(0, "rowid".into());
        }
        for row in rows(conn, &select) {
            let values: Vec<String> = row.iter().map(render).collect();
            writeln!(
                out,
                "INSERT INTO {name}({}) VALUES({});",
                names.join(","),
                values.join(",")
            )
            .unwrap();
        }
    }
    for row in rows(
        conn,
        "SELECT sql FROM sqlite_schema WHERE type IN ('index','trigger','view') AND sql IS NOT NULL ORDER BY rowid",
    ) {
        if let Sql::Text(sql) = &row[0] {
            writeln!(out, "{sql};").unwrap();
        }
    }
    writeln!(out, "PRAGMA user_version={version};").unwrap();
    out
}

fn version_of(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// Writes `fixtures/migrations/vNN.sql` for every schema the library still
/// opens. Not a check: run by hand when fixtures are added.
#[test]
#[ignore]
fn write_historical_catalogs() {
    if std::env::var_os("RAWTONIC_WRITE_MIGRATION_FIXTURES").is_none() {
        return;
    }
    fs::create_dir_all(fixtures()).unwrap();
    for version in 1..=DB_SCHEMA_VERSION as usize {
        let conn = schema_at(version);
        assert_eq!(version_of(&conn) as usize, version);
        fill(&conn, version);
        fs::write(
            fixtures().join(format!("v{version:02}.sql")),
            dump(&conn, version),
        )
        .unwrap();
    }
}

/// A session whose database is fixture `version`, opened (and so
/// upgraded) by the library.
fn upgraded(dir: &Path, version: u32) -> Library {
    let job = dir.join("Job");
    drop(Library::create(&job, "Job", Mode::Session).unwrap());
    for suffix in ["", "-wal", "-shm"] {
        let path = job.join(format!("session.sqlite{suffix}"));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    let sql = fs::read_to_string(fixtures().join(format!("v{version:02}.sql"))).unwrap();
    let conn = Connection::open(job.join("session.sqlite")).unwrap();
    conn.execute_batch(&format!("BEGIN;{sql}COMMIT;")).unwrap();
    assert_eq!(version_of(&conn), version);
    drop(conn);
    Library::open(&job).unwrap()
}

/// The schema as `sqlite_schema` holds it, sorted.
fn schema(conn: &Connection) -> String {
    let mut out = String::new();
    for row in rows(
        conn,
        "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name",
    ) {
        let values: Vec<String> = row.iter().map(render).collect();
        writeln!(out, "{}", values.join(" | ")).unwrap();
    }
    out
}

/// The fixed set of library searches whose results must survive a
/// migration.
fn searches() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "text contains 0123",
            json!({"field": "text", "op": "contains", "value": "0123"}),
        ),
        (
            "text contains kiss",
            json!({"field": "text", "op": "contains", "value": "kiss"}),
        ),
        (
            "keyword is wedding",
            json!({"field": "keyword", "op": "is", "value": "wedding"}),
        ),
        (
            "keyword is sunset",
            json!({"field": "keyword", "op": "is", "value": "sunset"}),
        ),
        (
            "camera is ILCE-7RM4",
            json!({"field": "camera", "op": "is", "value": "ILCE-7RM4"}),
        ),
        (
            "make is sony",
            json!({"field": "make", "op": "is", "value": "sony"}),
        ),
        (
            "lens starts with FE",
            json!({"field": "lens", "op": "startsWith", "value": "FE"}),
        ),
        (
            "file type jpeg or dng",
            json!({"field": "fileType", "op": "is", "value": ["jpeg", "dng"]}),
        ),
        (
            "rating at least 3",
            json!({"field": "rating", "op": "ge", "value": 3}),
        ),
        (
            "picked",
            json!({"field": "flag", "op": "is", "value": "picked"}),
        ),
        (
            "label red or green",
            json!({"field": "label", "op": "is", "value": ["red", "green"]}),
        ),
        (
            "iso at least 800",
            json!({"field": "iso", "op": "ge", "value": 800}),
        ),
        (
            "captured in June 2026",
            json!({"field": "captureDate", "op": "between", "value": ["2026-06-01", "2026-06-30"]}),
        ),
        (
            "imported 2026-06-01 to 03",
            json!({"field": "importDate", "op": "between", "value": ["2026-06-01", "2026-06-03"]}),
        ),
        (
            "album Wedding",
            json!({"field": "album", "op": "contains", "value": ALBUM}),
        ),
        (
            "group Clients",
            json!({"field": "album", "op": "contains", "value": GROUP}),
        ),
        (
            "smart album",
            json!({"field": "album", "op": "contains", "value": SMART_ALBUM}),
        ),
        (
            "previous import",
            json!({"field": "previousImport", "op": "is", "value": true}),
        ),
        (
            "offline",
            json!({"field": "offline", "op": "is", "value": true}),
        ),
        (
            "AI Denoise",
            json!({"field": "ai", "op": "is", "value": "aiDenoise"}),
        ),
        (
            "generated areas",
            json!({"field": "ai", "op": "is", "value": "generated"}),
        ),
        (
            "source type",
            json!({"field": "sourceType", "op": "notEmpty"}),
        ),
        (
            "duplicates",
            json!({"field": "duplicate", "op": "is", "value": true}),
        ),
        (
            "eyes closed",
            json!({"field": "eyes", "op": "is", "value": "closed"}),
        ),
        (
            "none of: keyword wedding, rating 0",
            json!({"combine": "none", "rules": [
                {"field": "keyword", "op": "is", "value": "wedding"},
                {"field": "rating", "op": "is", "value": 0}]}),
        ),
    ]
}

/// What an upgraded fixture holds: schema, rows of every table (sorted),
/// the foreign-key check and the searches' results.
fn report(lib: &Library, db: &Path) -> String {
    let conn = Connection::open(db).unwrap();
    let mut out = format!(
        "# user_version {}\n# schema\n{}",
        version_of(&conn),
        schema(&conn)
    );
    for (name, _) in data_tables(&conn) {
        let mut lines: Vec<String> = rows(&conn, &format!("SELECT * FROM {name}"))
            .iter()
            .map(|row| row.iter().map(render).collect::<Vec<_>>().join(" | "))
            .collect();
        lines.sort();
        writeln!(
            out,
            "# table {name} ({} rows): {}",
            lines.len(),
            columns(&conn, &name).join(", ")
        )
        .unwrap();
        for line in lines {
            writeln!(out, "{line}").unwrap();
        }
    }
    writeln!(out, "# foreign_key_check").unwrap();
    for row in rows(&conn, "PRAGMA foreign_key_check") {
        writeln!(
            out,
            "{}",
            row.iter().map(render).collect::<Vec<_>>().join(" | ")
        )
        .unwrap();
    }
    writeln!(out, "# searches").unwrap();
    for (name, rule) in searches() {
        let rule: Rule = serde_json::from_value(rule).unwrap();
        match lib.search_ids(&rule) {
            Ok(ids) => writeln!(out, "{name}: {ids:?}").unwrap(),
            Err(e) => writeln!(out, "{name}: error {e:#}").unwrap(),
        }
    }
    out
}

#[test]
fn every_historical_catalog_upgrades_to_its_recorded_result() {
    let bless = std::env::var_os("RAWTONIC_BLESS_MIGRATIONS").is_some();
    let mut failed = Vec::new();
    for version in 1..=DB_SCHEMA_VERSION {
        let dir = tempfile::tempdir().unwrap();
        let lib = upgraded(dir.path(), version);
        let got = report(&lib, &dir.path().join("Job/session.sqlite"));
        let path = fixtures().join(format!("v{version:02}.expected"));
        if bless {
            fs::write(&path, &got).unwrap();
            continue;
        }
        let want = fs::read_to_string(&path).unwrap();
        if got != want {
            let first = got
                .lines()
                .zip(want.lines())
                .find(|(g, w)| g != w)
                .map(|(g, w)| format!("got  {g}\nwant {w}"))
                .unwrap_or_else(|| "lengths differ".into());
            failed.push(format!("v{version:02}: {first}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn every_upgraded_schema_is_the_schema_of_a_new_library() {
    let dir = tempfile::tempdir().unwrap();
    drop(Library::create(&dir.path().join("New"), "New", Mode::Session).unwrap());
    let fresh = schema(&Connection::open(dir.path().join("New/session.sqlite")).unwrap());
    for version in 1..=DB_SCHEMA_VERSION {
        let dir = tempfile::tempdir().unwrap();
        drop(upgraded(dir.path(), version));
        let conn = Connection::open(dir.path().join("Job/session.sqlite")).unwrap();
        assert_eq!(version_of(&conn), DB_SCHEMA_VERSION);
        assert_eq!(schema(&conn), fresh, "schema {version} upgraded");
    }
}

/// A migration that fails part-way leaves the library as it was: every step
/// of the upgrade is one transaction, so no step's change is kept.
#[test]
fn a_failing_migration_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("Job");
    drop(Library::create(&job, "Job", Mode::Session).unwrap());
    let sql = fs::read_to_string(fixtures().join("v10.sql")).unwrap();
    let db = job.join("session.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let path = job.join(format!("session.sqlite{suffix}"));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    let conn = Connection::open(&db).unwrap();
    // Schema 11 rebuilds the albums through this name: its script fails.
    conn.execute_batch(&format!(
        "BEGIN;{sql}CREATE TABLE collections_v11(id TEXT);COMMIT;"
    ))
    .unwrap();
    let before = dump(&conn, 10);
    drop(conn);
    assert!(Library::open(&job).is_err());
    let conn = Connection::open(&db).unwrap();
    assert_eq!(version_of(&conn), 10);
    assert_eq!(dump(&conn, 10), before);
}

/// Schema 26: AI Denoise edits saved before version 4 are listed as older
/// (the fact was computed when they were saved, with 3 the latest); the
/// recipes are not touched, and the step gives the same facts again.
#[test]
fn schema_26_flags_ai_denoise_before_version_4_and_only_facts() {
    use rawtonic_metadata::provenance::{AI_DENOISE, OLDER_DENOISE};
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("Job");
    drop(Library::create(&job, "Job", Mode::Session).unwrap());
    for suffix in ["", "-wal", "-shm"] {
        let path = job.join(format!("session.sqlite{suffix}"));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    let db = job.join("session.sqlite");
    let conn = Connection::open(&db).unwrap();
    register_functions(&conn);
    let sql = fs::read_to_string(fixtures().join("v25.sql")).unwrap();
    conn.execute_batch(&format!("BEGIN;{sql}COMMIT;")).unwrap();
    // (version id, head recipe, legacy editor state, facts an earlier build stored)
    let adjustments = |extra: serde_json::Value| {
        let mut a = json!({"exposure": 0.2});
        a.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"adjustments": a}).to_string()
    };
    let cases = [
        (901, adjustments(json!({"aiDenoise": 40})), true), // no version: 1
        (902, adjustments(json!({"aiDenoise": 40, "aiDenoiseVersion": 2})), true),
        (903, adjustments(json!({"aiDenoise": 40, "aiDenoiseVersion": 3})), true),
        (904, adjustments(json!({"aiDenoise": 40, "aiDenoiseVersion": 4})), true),
        (905, adjustments(json!({"aiDenoise": 0, "aiDenoiseVersion": 3})), false),
        (906, adjustments(json!({"exposure": 1.0})), false),
    ];
    for (n, payload, _) in &cases {
        let id = *n as u32;
        put(
            &conn,
            "assets",
            &[
                ("id", t(&asset_id(id))),
                ("root_id", t("session")),
                ("rel_path", t(&format!("Capture/A{n}.ARW"))),
                ("size", i(1)),
                ("mtime", i(1)),
                ("fingerprint", t(&format!("xxh3:{n}"))),
                ("kind", t("arw")),
            ],
        );
        put(
            &conn,
            "versions",
            &[
                ("id", t(&version_id(id))),
                ("asset_id", t(&asset_id(id))),
                ("name", t("Original")),
                ("is_primary", i(1)),
            ],
        );
        put(
            &conn,
            "recipes",
            &[
                ("id", i(1000 + n)),
                ("version_id", t(&version_id(id))),
                ("schema_version", i(1)),
                ("payload", t(payload)),
                ("created_at", t("2026-06-01 12:00:00")),
            ],
        );
        conn.execute(
            "UPDATE versions SET head=? WHERE id=?",
            rusqlite::params![1000 + n, version_id(id)],
        )
        .unwrap();
    }
    // A virtual copy keeps its editor state instead of a recipe.
    let copy = adjustments(json!({"aiDenoise": 40, "aiDenoiseVersion": 3}));
    let copy_state = json!({"editor_state": serde_json::from_str::<serde_json::Value>(&copy).unwrap()})
        .to_string();
    conn.execute(
        "INSERT INTO versions(id, asset_id, name, is_primary, legacy_state) VALUES(?,?,?,0,?)",
        rusqlite::params![version_id(907), asset_id(901), "Copy", copy_state],
    )
    .unwrap();
    // As earlier builds stored them: AI Denoise, never the older bit.
    conn.execute(
        "UPDATE versions SET ai_facts = ai_facts & ~? WHERE ai_facts != 0",
        [i64::from(OLDER_DENOISE)],
    )
    .unwrap();
    let facts = |conn: &Connection| -> Vec<(u32, i64)> {
        (901..=907)
            .map(|n| {
                (
                    n,
                    conn.query_row(
                        "SELECT ai_facts FROM versions WHERE id=?",
                        [version_id(n)],
                        |r| r.get(0),
                    )
                    .unwrap(),
                )
            })
            .collect()
    };
    let (denoise, older) = (i64::from(AI_DENOISE), i64::from(AI_DENOISE | OLDER_DENOISE));
    assert_eq!(
        facts(&conn).iter().map(|f| f.1).collect::<Vec<_>>(),
        [denoise, denoise, denoise, denoise, 0, 0, denoise],
        "stored as an earlier build left them"
    );
    let recipes_before = rows(&conn, "SELECT id, payload FROM recipes ORDER BY id");
    drop(conn);

    drop(Library::open(&job).unwrap());
    let conn = Connection::open(&db).unwrap();
    register_functions(&conn);
    assert_eq!(version_of(&conn), DB_SCHEMA_VERSION);
    let after = facts(&conn);
    assert_eq!(
        after.iter().map(|f| f.1).collect::<Vec<_>>(),
        [older, older, older, denoise, 0, 0, older],
        "versions 1, 2, 3 (and the copy) are older; 4, off and unedited are not"
    );
    assert_eq!(
        rows(&conn, "SELECT id, payload FROM recipes ORDER BY id"),
        recipes_before,
        "no recipe changed"
    );
    conn.execute_batch(&script(26)).unwrap();
    assert_eq!(facts(&conn), after, "running it again changes nothing");
}

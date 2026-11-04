# `state/` — managed sub-states of the desktop adapter

`AppState` is being split into one managed sub-state per concern (plan step 3.3). Each lives
here, is registered with `.manage(...)` in `lib.rs`, and is reached through its own module's
functions, not through `AppState`.

| Sub-state | Holds | Access |
|---|---|---|
| `settings::SettingsStore` | `settings.json`, read + migrated once per run | `settings::current(app)` / `try_current(app)` (cheap `Arc`), `settings::update(app, \|s\| …)` |
| `develop::DevelopSession` | the caches built from the open photograph (decodes, preview bases, GPU upload, masks, warps, geometry bases, the editor's patches) | `app.state::<DevelopSession>()`, passed on as `&DevelopSession`; dropped only through `invalidate(Scope)` |

## Lock policy

- Code in `state/` locks only through `lock::lock_recover` (`Mutex`) and
  `lock::read_recover` / `lock::write_recover` (`RwLock`): a lock poisoned by a panic in another
  thread is recovered with its data and logged once per run (the first one as an error). No
  `.lock().unwrap()`, no `if let Ok(...) = lock()`; the rest of the adapter is converted in later
  parts of 3.3.
- Hold a lock for one short step; never across an `.await`, a Tauri call or an event emit.
- Read-modify-write runs under one lock from read to write (`SettingsStore::update`), so
  concurrent changes cannot lose one another.

## Lock order

Take locks only in this order (a later one may be taken while an earlier one is held, never
the reverse):

1. `SettingsStore.writer` (held for a whole load or update, including the atomic file write)
2. `SettingsStore.current` (held only to swap or clone the `Arc`)
3. `app_settings::UNKEPT_SETTINGS` (inside `persist_settings_file`)

`save_settings` takes `AppState.decoded_image_cache` only after the store's locks are released.
Readers (`current`) take only `current` for reading, except on the first read of a run.

### `DevelopSession`

`invalidate(scope)` takes each cache's lock alone, one after the other, in field order, and the
caller holds no `DevelopSession` lock (except `cached_preview` for `Scope::PreviewUpload`, which
takes only `gpu_image_cache`). `Scope::Decoder` then takes the multi view's prefetch queue and held
decodes, and the proxy worker's kept decodes (`forget_decodes`, `forget_ahead`), with no
`DevelopSession` lock held.

Nested locks in use (earlier one held while the later one is taken):

1. `AppState.original_image` → `full_warped_cache` (publishing a warp built from the source still
   open, `editor_preview::full_warped_image`)
2. `geometry_cache` → `AppState.original_image` (negative preview base,
   `negative_conversion::preview_negative_conversion`); nothing takes `geometry_cache` while
   holding `original_image`.
3. `cached_preview` → `reusable_previews`, `full_transformed_cache`, `patched_warped_cache`,
   `gpu_image_cache` (building the preview base under its lock, `editor_preview::process_preview_job`;
   each taken alone).

Every other access holds one `DevelopSession` lock for one step. The sites outside `state/` keep
their lock calls (`lock().unwrap()` or recovery) until their context's part of 3.3; `invalidate`
recovers poisoned locks (`lock_recover`), where `clear_image_caches` skipped a poisoned one.

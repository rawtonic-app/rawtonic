# `state/` — managed sub-states of the desktop adapter

`AppState` is being split into one managed sub-state per concern (plan step 3.3). Each lives
here, is registered with `.manage(...)` in `lib.rs`, and is reached through its own module's
functions, not through `AppState`.

| Sub-state | Holds | Access |
|---|---|---|
| `settings::SettingsStore` | `settings.json`, read + migrated once per run | `settings::current(app)` / `try_current(app)` (cheap `Arc`), `settings::update(app, \|s\| …)` |
| `develop::DevelopSession` | the caches built from the open photograph (decodes, preview bases, GPU upload, masks, warps, geometry bases, the editor's patches) | `app.state::<DevelopSession>()`, passed on as `&DevelopSession`; dropped only through `invalidate(Scope)` |
| `thumbnails::ThumbnailState` | the Library's thumbnail queue and its workers' bookkeeping (`ThumbnailManager`), the cancel flag, the progress counter, the thumbnail bases, the restoration slot | `app.state::<ThumbnailState>()`, passed on as `&ThumbnailState`; progress through `reset_progress` / `add_to_total` / `set_queued` / `complete_one` |
| `ai::AiState` | the AI models and their cached results (`ai_processing::AiModels`), the locks that serialise loading and running them, their lifecycle (use, running computations, unloads), AI Denoise, faces, the running AI tasks and the tagging pass | `app.state::<AiState>()`, passed on as `&AiState` (the model loaders in `ai_processing` take it) |

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

### `ThumbnailState`

1. `manager.queue` (a worker taking the next photo; `update_thumbnail_queue` replacing it)
2. `manager.processing_now` (taken under `queue` to skip a photo already in flight)
3. `progress` (taken under both when a worker skips a photo already in flight, and under `queue`
   when an empty viewport clears it; each progress method holds it for one step only)

`manager.io_gate` and `geometry_cache` are each taken alone. `cancelled` is an atomic: set by
`cancel_thumbnail_generation`, cleared when `update_thumbnail_queue` starts a new generation, read
by the workers under `queue` (`take_next_thumbnail_path`, which then drops the rest of the queue).
A worker that skips a photo already in flight emits its progress event while it still holds
`queue` and `processing_now` (as before the split); no listener takes these locks.

### `AiState`

The tokio locks are held across the awaits of one load or run:

1. a run lock: `sam_encode_lock` (one SAM encode), `subject_lock` / `sky_lock` (one Subject or Sky
   run, `AutoMask::lock`)
2. a load lock: `init_lock` (the models that share it), `sky_init_lock`, `moebius_init_lock`; taken
   only inside the loaders, which take no run lock
3. `models`, `faces` or `denoisers` (the std locks, each held for one step: a look-up or a store)
4. `lifecycle`'s usage lock (inside `keep`, under `models` or `faces` when a loaded model is kept)

`tasks` and `indexing_task` are each taken alone. `ai_lifecycle::unload` takes `models`, then
`faces`, one after the other, and frees the dropped models outside both. A computation's
`Lifecycle::run` guard owns an `Arc` of the lifecycle, so it can go into a blocking task.


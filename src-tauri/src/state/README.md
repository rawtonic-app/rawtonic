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
| `library_files::AlbumsStore` | `albums.json`: the app's album list, read once per run, every change saved under one write lock | `app.state::<AlbumsStore>()`, `get(app)` / `update(app, \|albums\| …)` (through `file_management`; the file operation sync uses it too) |
| `library_files::PresetsStore` | `presets.json`: the user's presets, read once per run, every change saved under one write lock | `app.state::<PresetsStore>()`, `get(app)` / `update(app, \|presets\| …)` (through `file_management`) |
| `library_files::LibraryQueue` | ordered album/preset changes, pending work and the last failed change per file | `app.state::<LibraryQueue>()`; `run_change` and `flush_library` |
| `export::ExportState` | the export queue (`export-queue.sqlite`), the jobs the runner works on, whether it runs and whether the queue is held, the Edit With runs a quit cut short, the running export task's cancel token | `app.state::<ExportState>()`, passed on as `&ExportState` (`export_queue::with_queue`, `snapshot`, `hold`, `has_unfinished_job_in`, `session_move::{offer, busy, before_reversal}`) |

## Lock policy

- Code in `state/` locks only through `lock::lock_recover` (`Mutex`) and
  `lock::read_recover` / `lock::write_recover` (`RwLock`): a lock poisoned by a panic in another
  thread is recovered with its data and logged once per run (the first one as an error). No
  `.lock().unwrap()`, no `if let Ok(...) = lock()`; the rest of the adapter is converted in later
  parts of 3.3.
- Hold a lock for one short step; never across an `.await`, a Tauri call or an event emit.
- Read-modify-write runs under one lock from read to write (`SettingsStore::update`,
  `AlbumsStore::update`, `PresetsStore::update`), so concurrent changes cannot lose one another.

## Lock order

Take locks only in this order (a later one may be taken while an earlier one is held, never
the reverse):

1. `SettingsStore.writer` (held for a whole load or update, including the atomic file write)
2. `SettingsStore.current` (held only to swap or clone the `Arc`)
3. `app_settings::UNKEPT_SETTINGS` (inside `persist_settings_file`)

`AlbumsStore` / `PresetsStore` (`state::library_files`) follow the same order within each store:
`writer`, then `current`. They are never held together with each other or with the settings' locks
(no path takes a library file's lock while holding the settings' or the other file's).

`save_settings` takes `AppState.decoded_image_cache` only after the store's locks are released.
Readers (`current`) take only `current` for reading, except on the first read of a run.

### Library files

`AlbumsStore` and `PresetsStore` hold one file each, with the settings' lock order within them (`writer`,
then `current`, held for a whole read-modify-write including the atomic file write). A change waits on the
file's write lock, never on another store's.

`LibraryQueue` has one mutex and one condvar. A command records its pending change and waits for
its predecessors for up to `ENTRY_GRACE` (30 s), then releases the queue lock before taking a
store's locks. The condvar is always waited on with a timeout. A timed-out change returns an error
without running its work; the predecessor keeps its turn. It records the outcome only after
releasing the store's locks. A flush waits up to `FLUSH_TIMEOUT` (10 s) for the real pending
changes received before it, including imports still parsing files; a flush never becomes pending work
itself. Failures are kept per file until reported; only a successful whole-list preset save clears
its file's older failure. Album edits and preset imports cannot repair an earlier failed edit.
An edit rejected because its target album is gone returns an error to its caller without recording
a save failure. Imports parse and merge in their one ordered change; the frontend sends one command at a time.

The touched photographs are all read and the sidecar writes all done before the lock is taken (an album
change waits on no file of the photographs); and no code takes one of the stores' locks while holding
another of these: the file operations' album sync and `add_to_album` run on blocking workers, the albums'
commands hand their store work to a blocking worker and return, and the presets' imports parse the
imported files before they take the lock. The main thread never waits on a library file's write lock.

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

### `ExportState`

1. `running_jobs` (`with_running`: held while a pause or cancel acts on a running job, so the runner
   cannot move on meanwhile; the runner sets and clears it around each export task)
2. `task_token` (a cancel of a job running alone signals the export task under `running_jobs`,
   `export_processing::cancel_running_export`)

`queue` is taken alone, for one queue call (`with_queue`); never while `running_jobs` is held.
`interrupted_edits` is taken alone. `runner_active` and `held` are atomics. A pause emits
`export-job-pausing` under `running_jobs` (as before the split; no listener takes it).

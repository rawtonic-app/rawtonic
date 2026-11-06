# `state/` — managed sub-states of the desktop adapter

`AppState` is being split into one managed sub-state per concern (plan step 3.3). Each lives
here, is registered with `.manage(...)` in `lib.rs`, and is reached through its own module's
functions, not through `AppState`.

| Sub-state                     | Holds                                                                                                                                                                                                                                                                       | Access                                                                                                                                                                                                                                        |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `settings::SettingsStore`     | `settings.json`, read + migrated once per run                                                                                                                                                                                                                               | `settings::current(app)` / `try_current(app)` (cheap `Arc`), `settings::update(app, \|s\| …)`                                                                                                                                                 |
| `develop::DevelopSession`     | the caches built from the open photograph (decodes, preview bases, GPU upload, masks, warps, geometry bases, the editor's patches)                                                                                                                                          | `app.state::<DevelopSession>()`, passed on as `&DevelopSession`; dropped only through `invalidate(Scope)`                                                                                                                                     |
| `thumbnails::ThumbnailState`  | the Library's thumbnail queue and its workers' bookkeeping (`ThumbnailManager`), the cancel flag, the progress counter, the thumbnail bases, the restoration slot                                                                                                           | `app.state::<ThumbnailState>()`, passed on as `&ThumbnailState`; progress through `reset_progress` / `add_to_total` / `set_queued` / `complete_one`                                                                                           |
| `ai::AiState`                 | the AI models and their cached results (`ai_processing::AiModels`), the locks that serialise loading and running them, their lifecycle (use, running computations, unloads), AI Denoise, faces, the running AI tasks and the tagging pass                                   | `app.state::<AiState>()`, passed on as `&AiState` (the model loaders in `ai_processing` take it)                                                                                                                                              |
| `library_files::AlbumsStore`  | `albums.json`: the app's album list, read once per run, every change saved under one write lock                                                                                                                                                                             | `app.state::<AlbumsStore>()`, `get(app)` / `update(app, \|albums\| …)` (through `file_management`; the file operation sync uses it too)                                                                                                       |
| `library_files::PresetsStore` | `presets.json`: the user's presets, read once per run, every change saved under one write lock                                                                                                                                                                              | `app.state::<PresetsStore>()`, `get(app)` / `update(app, \|presets\| …)` (through `file_management`)                                                                                                                                          |
| `library_files::LibraryQueue` | ordered album/preset changes, pending work and the last failed change per kind                                                                                                                                                                                              | `app.state::<LibraryQueue>()`; `run_change` and `flush_library`                                                                                                                                                                               |
| `export::ExportState`         | the export queue (`export-queue.sqlite`), the jobs the runner works on, whether it runs and whether the queue is held, the Edit With runs a quit cut short, the running export task's cancel token                                                                          | `app.state::<ExportState>()`, passed on as `&ExportState` (`export_queue::with_queue`, `snapshot`, `hold`, `has_unfinished_job_in`, `session_move::{offer, busy, before_reversal}`)                                                           |
| `startup::StartupState`       | the first-window handshake, the initial file path and pending external edit session                                                                                                                                                                                         | `app.state::<StartupState>()`; `frontend_ready` takes `State<StartupState>`                                                                                                                                                                   |
| `library::LibraryState`       | the metadata worker manager and the disk-kind cache used by Library listings                                                                                                                                                                                                | `app.state::<LibraryState>()` in the listing helpers and startup                                                                                                                                                                              |
| `merge::MergeState`           | the merge dialog's result, current job and run lock                                                                                                                                                                                                                         | merge commands take `State<MergeState>`; source invalidation takes `&MergeState`                                                                                                                                                              |
| `gpu::GpuState`               | the GPU processing context and the instance/adapter it was made from, the display processor and the exports' own processor, the scopes' GPU upload, the device start's crash flag path, why the GPU did not start, the window's newest size                                 | `app.state::<GpuState>()`, passed on as `&GpuState` (`gpu_processing::get_or_init_gpu_context`, `attach_display`, `resize_display`, the `process_and_get_dynamic_image*` wrappers); `context()` for the context if one exists                 |
| `AppState` (`app_state.rs`)   | the editor's state, two parts: `source` (`source::SourceState`) and `preview` (`preview::PreviewState`); the editor's commands take it as `State<AppState>` (those pinned by `src/contracts/commandErrors.test.ts` keep that signature)                                     | `app.state::<AppState>()`; helpers that need one part take `&SourceState` (`image_loader::original_image`, `wait_for_original_image`, `open_source`, `clear_source_caches`) or `&PreviewState` (`scope_analytics::next_generation`, `sender`) |
| `source::SourceState`         | the open photograph's source: its decode (`original_image`), its preview proxy while the decode runs and the proxies kept in memory, whether it is offline, the photograph the editor asked for last, the load generation, and the signals for a changed or rendered source | `AppState.source`; written by `image_loader` (and the proxy worker's kept decodes), read by the preview, the AI tools and exports of the open photograph                                                                                      |
| `preview::PreviewState`       | the preview worker's next job and its generation, the last full preview's size, the brush stroke being drawn, the scopes' workers, their generations and the scope render's small copy of the base                                                                          | `AppState.preview`; `editor_preview`, `scope_analytics`                                                                                                                                                                                       |
| `luts::LutState`              | the LUT files parsed this run, by path and file stamp                                                                                                                                                                                                                       | `app.state::<LutState>()`, passed on as `&LutState` (`lut_processing::get_or_load_lut`, `load_and_parse_lut_blocking`): preview, thumbnails, multi view, collages, exports                                                                    |

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
itself. Failures are kept per kind of change (album edit, preset save, preset import) until reported;
only a successful whole-list preset save clears the older failed preset save. Album edits and
preset imports cannot repair an earlier failed edit, and a save does not hold the presets of a
failed import.
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

1. `SourceState.original_image` → `full_warped_cache` (publishing a warp built from the source still
   open, `editor_preview::full_warped_image`)
2. `geometry_cache` → `SourceState.original_image` (negative preview base,
   `negative_conversion::preview_negative_conversion`); nothing takes `geometry_cache` while
   holding `original_image`.
3. `SourceState.current_source` → `patch_cache` (`image_loader::open_source` makes another
   photograph current under its lock and drops the previous one's bitmaps,
   `invalidate(Scope::OtherPhoto)`).
4. `cached_preview` → `reusable_previews`, `full_transformed_cache`, `patched_warped_cache`,
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

### `StartupState`

`initial_file_path` and `pending_edit_session` are each taken alone, for a store or a take.
`window_setup_complete` is atomic. No startup lock is held during window calls or an event emit.

### `LibraryState`

The metadata manager's `pending` (deduplication) and `queue` locks are taken separately, never
nested: enqueue records the pending sidecar, releases `pending`, then appends to `queue`; a
worker pops under `queue`, releases it, reads the metadata and emits its event, then removes
from `pending`. The queue's condvar wait recovers a poisoned guard too.

`disks_cache` is taken alone for a lookup or replacement, and released before accessing
`ThumbnailState` or querying disks. `disks_cache_refreshing` is atomic. None of these locks
is held together with a settings or library-file store's lock.

### `MergeState`

1. `running` (tokio mutex, held across one merge's blocking computation and its await)
2. `result` (std mutex; publishing the result takes it while `running` is held)

A new merge clears `result` and releases it before waiting for `running`; no path takes
`running` while holding `result`. `current_job` is atomic. Refinish holds `result` while it
updates the outline and computes its preview on a blocking worker, as before the split.
Save takes the result out before encoding or writing; a failed publish may put it back
under the same lock if its job is still current. Events are emitted outside `result`.
Source invalidation clears `DevelopSession` first (its locks released), then takes `result`
alone; it does not change the current job, matching the previous cache invalidation.

### `GpuState`

1. `processor` (a `ProcessorSlot`; `attach_display` holds it while it replaces the context, so no render uses
   the old processor with the new device)
2. `context` (`context_slot` for creating the context, `context_lock` for replacing it or using its display
   under it, `context()` for a clone)
3. `adapter` (stored under `context` when a context is created or replaced; read alone by `attach_display`)
4. the context's `display` (`GpuContext::display`; taken under `context` by the sites that place or present
   on the display, as before the split)

`context` keeps its own poisoning policy: a lock poisoned by a panic during a device start is emptied (the next
use starts a new device), not recovered with its data (`context_slot`; `context_lock` and `context()` recover).
`crash_flag_path` is read under `context` while a device starts (`create_context`), for one read.
`export_processor`, `scope_cache` and `start_error` are each taken alone. `window_size` is an atomic (an `Arc`, so the worker that waits for the display can own it).

### `SourceState` (`AppState.source`)

1. `original_image` → `current_source` (`image_loader::open_kept_decode` checks under the source lock
   that the photograph is still the one asked for, so a later load's source cannot be replaced)
2. `current_source` → `DevelopSession.patch_cache` (`open_source`, see `DevelopSession`)
3. `source_signal`'s change counter → `original_image`, `offline_source`, `proxy_image`
   (`SourceSignal::wait_for` holds it while its check reads the slots, each alone, so a change cannot
   slip between check and wait); `changed()` is called with the slots' locks released

`proxy_image`, `offline_source`, `proxy_memory` and `rendered_source`'s lock are each taken alone,
for one step. `load_image_generation` is an atomic (an `Arc`, so a load's tracker can own it). The
sites keep their lock calls (`unwrap_or_else(into_inner)` recovery, as before the split).

### `PreviewState` (`AppState.preview`)

`preview_jobs` (its slot's own lock), `live_stroke`, `analytics_worker_tx`, `scope_analytics_tx` and
`scope_proxy` are each taken alone, for one step. `preview_generation`, `last_preview_dim`,
`analytics_generation` and `full_frame_analytics_generation` are atomics, with the orderings of
before the split.

### `LutState`

One lock (`cache()`, `lock_recover`), held for one look-up (a parse of a changed file included, as
before) or one store; no other lock is taken under it.

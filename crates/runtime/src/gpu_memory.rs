//! The error GPU work reports when the graphics card has no memory left
//! for it (a wgpu `OutOfMemory` caught by an error scope), and the retry
//! every export (app and CLI) gives a photo that met it. Another program (a
//! game, another editor, a GPU job) that holds the memory often frees it
//! within seconds; a batch export should not lose the photos that met it
//! (notes: benchmark-results/axf-app-export-flake-2026-11-10). Render
//! errors travel as text through the export paths; [`GPU_OUT_OF_MEMORY`]
//! is the one place that text is defined.

use std::time::Duration;

/// The start of every GPU out-of-memory error.
pub const GPU_OUT_OF_MEMORY: &str = "The graphics card ran out of memory for this photograph.";

/// What a photo still out of graphics memory after every retry fails with
/// (the app's `renderErrors.ts` translates it).
pub const GPU_MEMORY_EXHAUSTED: &str = "Not enough graphics memory for this photo. Close other programs that use the graphics card and export this photo again.";

/// What GPU work fails with once the graphics chip's device is lost (its
/// driver reset it, e.g. out of memory; G76/WCC6: a Radeon 680M lost it
/// compiling an export variant). Every later render on that device fails
/// too, and the app makes no new device while it runs, so the photo is
/// exported again after a restart (the app's `renderErrors.ts` translates
/// it).
pub const GPU_DEVICE_LOST: &str = "This photo could not be rendered on this graphics chip: it stopped responding (out of memory). Quit and reopen Rawtonic, then export this photo again.";

/// What an export fails with when its shader variant is too large for a
/// graphics driver that compiles under a limit (G76/WCC6: the Radeon 680M's
/// Windows Vulkan driver took 438 s and 52 GB for one and lost its device).
/// The app's `renderErrors.ts` translates it.
pub const GPU_EXPORT_TOO_COMPLEX: &str = "This photo's edits are too complex for this graphics chip's driver to export. Remove some edits (effects, local adjustments) and export it again, or export it on another computer.";

/// The waits before the retries: about 7.5 s at most per photo, so a card
/// that stays full fails each photo in turn instead of stalling the batch.
pub const RETRY_WAITS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// The error of a GPU pass (`what`) that ran out of graphics memory.
pub fn gpu_out_of_memory(what: &str) -> String {
    format!("{GPU_OUT_OF_MEMORY} Close other programs that use it and try again. ({what})")
}

/// Whether `error` is (or wraps) a GPU out-of-memory error.
pub fn is_gpu_out_of_memory(error: &str) -> bool {
    error.contains(GPU_OUT_OF_MEMORY)
}

/// Exports `wanted` (a photo's recipes) with `attempt` and exports those
/// that ran out of graphics memory again after each of [`RETRY_WAITS`]:
/// `before_retry` gets them and the wait, frees what memory it can, waits,
/// and says `false` when the export was cancelled meanwhile. Only those
/// `may_retry` allows go again (their file is not published). Those still
/// out of memory fail with [`GPU_MEMORY_EXHAUSTED`]: reported, never
/// skipped. Each one's result once, in the order `attempt` first gave.
pub fn retry_out_of_memory<K: Copy + PartialEq>(
    wanted: &[K],
    mut attempt: impl FnMut(&[K]) -> Vec<(K, Result<(), String>)>,
    mut may_retry: impl FnMut(K) -> bool,
    mut before_retry: impl FnMut(&[K], Duration) -> bool,
) -> Vec<(K, Result<(), String>)> {
    let out_of_memory =
        |result: &Result<(), String>| matches!(result, Err(e) if is_gpu_out_of_memory(e));
    let mut results = attempt(wanted);
    for wait in RETRY_WAITS {
        let again: Vec<K> = results
            .iter()
            .filter(|(key, result)| out_of_memory(result) && may_retry(*key))
            .map(|(key, _)| *key)
            .collect();
        if again.is_empty() || !before_retry(&again, wait) {
            break;
        }
        for (key, result) in attempt(&again) {
            if let Some(slot) = results.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = result;
            }
        }
    }
    for (_, result) in &mut results {
        if out_of_memory(result) {
            *result = Err(GPU_MEMORY_EXHAUSTED.to_string());
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oom() -> Result<(), String> {
        Err(gpu_out_of_memory("test"))
    }

    #[test]
    fn an_out_of_memory_error_is_known_with_any_context_around_it() {
        let error = gpu_out_of_memory("the warp");
        assert!(is_gpu_out_of_memory(&error));
        assert!(is_gpu_out_of_memory(&format!(
            "a.CR3: not exported, {error}"
        )));
        assert!(!is_gpu_out_of_memory(
            "The graphics card could not render this photograph: Validation Error"
        ));
    }

    #[test]
    fn recipes_out_of_graphics_memory_are_exported_again_after_a_wait() {
        let mut attempts = Vec::new();
        let mut waits = Vec::new();
        let results = retry_out_of_memory(
            &[0, 1, 2],
            |wanted| {
                attempts.push(wanted.to_vec());
                wanted
                    .iter()
                    .map(|&t| match (attempts.len(), t) {
                        (1, 1 | 2) | (2, 2) => (t, oom()),
                        _ => (t, Ok(())),
                    })
                    .collect()
            },
            |_| true,
            |_, wait| {
                waits.push(wait);
                true
            },
        );
        assert_eq!(attempts, [vec![0, 1, 2], vec![1, 2], vec![2]]);
        assert_eq!(waits, RETRY_WAITS[..2]);
        assert_eq!(results, [(0, Ok(())), (1, Ok(())), (2, Ok(()))]);
    }

    #[test]
    fn a_photo_still_out_of_graphics_memory_fails_with_a_clear_message() {
        let mut attempts = 0;
        let results = retry_out_of_memory(
            &[0, 1],
            |wanted| {
                attempts += 1;
                wanted
                    .iter()
                    .map(|&t| match t {
                        0 => (t, oom()),
                        _ => (t, Err("The file is damaged".to_string())),
                    })
                    .collect()
            },
            |_| true,
            |_, _| true,
        );
        assert_eq!(attempts, 1 + RETRY_WAITS.len());
        assert_eq!(
            results,
            [
                (0, Err(GPU_MEMORY_EXHAUSTED.to_string())),
                (1, Err("The file is damaged".to_string()))
            ],
            "other failures are not retried and keep their message"
        );
    }

    #[test]
    fn a_published_recipe_or_a_cancelled_export_is_not_exported_again() {
        let mut attempts = 0;
        let results = retry_out_of_memory(
            &[0],
            |wanted| {
                attempts += 1;
                wanted.iter().map(|&t| (t, oom())).collect()
            },
            |_| false,
            |_, _| true,
        );
        assert_eq!(attempts, 1);
        assert_eq!(results, [(0, Err(GPU_MEMORY_EXHAUSTED.to_string()))]);
        let mut attempts = 0;
        retry_out_of_memory(
            &[0],
            |wanted| {
                attempts += 1;
                wanted.iter().map(|&t| (t, oom())).collect()
            },
            |_| true,
            |_, _| false,
        );
        assert_eq!(attempts, 1, "cancelled during the wait");
    }
}

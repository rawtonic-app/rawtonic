//! The main window's size and place for the next start: noted on every
//! move or resize, written at most every `INTERVAL` while it changes, and
//! once more when the app quits (`flush`), so the last change is kept.
use crate::app_state::WindowState;
use std::{path::PathBuf, sync::Mutex, time::Duration};

/// How often a changed state is written while the window moves.
pub const INTERVAL: Duration = Duration::from_millis(500);

/// The newest state not written yet, and the file it goes to.
static PENDING: Mutex<Option<(PathBuf, WindowState)>> = Mutex::new(None);

/// Notes the window's current state (written by the next `flush`).
pub fn note(path: PathBuf, state: WindowState) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((path, state));
}

/// Maximized events carry screen bounds, never a new restore placement.
pub fn retain_normal_placement(state: &mut WindowState, normal: &mut Option<WindowState>) {
    if state.maximized {
        if let Some(saved) = normal {
            state.width = saved.width;
            state.height = saved.height;
            state.x = saved.x;
            state.y = saved.y;
            state.scale_factor = saved.scale_factor;
        }
    } else {
        *normal = Some(state.clone());
    }
}

/// Writes the newest noted state, if any. The lock is held while writing,
/// so a flush when quitting waits for a write in progress and the newest
/// state is the one left on disk.
pub fn flush() {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let Some((path, state)) = pending.take() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string(&state) {
        Ok(json) => {
            if let Err(e) = rawtonic_fs::atomic_write(&path, json) {
                log::warn!("Could not save the window state: {e}");
            }
        }
        Err(e) => log::warn!("Could not save the window state: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_state_is_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config/window_state.json");
        let state = |x| WindowState {
            width: 1400,
            height: 900,
            x,
            y: 40,
            maximized: false,
            scale_factor: Some(2.0),
            fullscreen: false,
        };
        let mut normal = Some(state(50));
        let mut maximized = state(0);
        maximized.maximized = true;
        maximized.width = 1280;
        retain_normal_placement(&mut maximized, &mut normal);
        assert_eq!(
            (maximized.x, maximized.width, maximized.height),
            (50, 1400, 900)
        );
        assert!(maximized.maximized);
        note(path.clone(), state(10));
        note(path.clone(), state(20));
        flush();
        let saved: WindowState =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((saved.x, saved.width), (20, 1400));
        assert_eq!(saved.scale_factor, Some(2.0));
        std::fs::remove_file(&path).unwrap();
        flush();
        assert!(!path.exists(), "nothing new to write");
    }
}

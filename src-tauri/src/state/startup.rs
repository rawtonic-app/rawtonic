//! The first-window handshake and launch request waiting for the frontend.
use crate::launch_request::ExternalEditSession;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

#[derive(Default)]
pub struct StartupState {
    pub window_setup_complete: AtomicBool,
    pub initial_file_path: Mutex<Option<String>>,
    pub pending_edit_session: Mutex<Option<ExternalEditSession>>,
}

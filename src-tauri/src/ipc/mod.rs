//! The IPC boundary: what commands send the frontend besides their results.

pub mod error;

pub use error::{CommandError, ErrorCode};

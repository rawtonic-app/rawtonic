//! The error a converted Tauri command rejects with: a machine-readable
//! `code` the frontend branches on and the `message` it shows. The message
//! is the text the command returned as a plain `String` before (the
//! frontend's `errorMessage` shows it unchanged), so only branching moves
//! from English text to `code` (`src/shared/ipc/errors.ts`).
//!
//! Serialized as `{ "code": "cancelled", "message": "Task cancelled" }`;
//! a `SaveFailure` keeps its `failed` and `culling` fields beside them.
//!
//! The code set is closed and small: a code is added only when a frontend
//! branch needs to tell that failure apart from the others.

use serde::Serialize;
use std::fmt;

/// What kind of failure a command reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The user, or a newer request for the same thing, stopped the work.
    Cancelled,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    Unsupported,
    Invalid,
    /// Other work holds what the command needs; it may succeed later.
    Busy,
    /// Anything the frontend does not tell apart.
    Internal,
    /// The request was for a photograph that is no longer the open one.
    Stale,
    /// The ONNX Runtime library is missing or unusable
    /// (`rawtonic_onnx::RuntimeError`).
    AiRuntimeUnavailable,
    /// The models a tool needs are not installed.
    ModelsMissing,
    /// The preview worker no longer holds mask parameters the request
    /// refers to; the frontend sends them in full again.
    MaskParametersMissing,
    /// A live brush frame does not continue the stroke the backend holds;
    /// the frontend starts the stroke over.
    LiveStrokeResync,
    /// The preview could not be rendered at all (e.g. an invalid shader).
    RenderFailed,
    /// Edit With refused photographs with generated areas; the message
    /// names them after `open_with::GENERATED_AREAS`.
    GeneratedAreas,
    /// A session cannot move while an export of it waits, runs or is
    /// paused (`session_move::EXPORTS_PENDING`); the Library offers the
    /// export queue.
    ExportsPending,
}

/// A command's failure: its `code` and the `message` the user sees.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    /// Fields of a structured error kept beside `code` and `message`
    /// (`SaveFailure`'s `failed` and `culling`).
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Cancelled, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// The same error with `key: value` beside code and message (data the
    /// frontend uses, such as the names of the photographs refused).
    pub fn with_detail(mut self, key: &str, value: serde_json::Value) -> Self {
        self.details
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), value);
        self
    }

    /// `error` with its whole cause chain in the message (`{error:#}`), as
    /// commands that formatted it so before show it.
    pub fn chain(error: anyhow::Error) -> Self {
        Self::new(code_of(&error), format!("{error:#}"))
    }

    /// The same error with `code` when `cancelled` (the command's own
    /// cancel flag or token was set when it failed): a failure after the
    /// user asked to stop is the stop, whatever step noticed it.
    pub fn or_cancelled(self, cancelled: bool) -> Self {
        if cancelled {
            Self {
                code: ErrorCode::Cancelled,
                ..self
            }
        } else {
            self
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn io_code(kind: std::io::ErrorKind) -> ErrorCode {
    use std::io::ErrorKind as K;
    match kind {
        K::NotFound => ErrorCode::NotFound,
        K::AlreadyExists => ErrorCode::AlreadyExists,
        K::PermissionDenied | K::ReadOnlyFilesystem => ErrorCode::PermissionDenied,
        K::Unsupported => ErrorCode::Unsupported,
        K::InvalidInput | K::InvalidData | K::InvalidFilename => ErrorCode::Invalid,
        // fileops reports a stopped operation as `Interrupted`.
        K::Interrupted => ErrorCode::Cancelled,
        K::WouldBlock | K::ResourceBusy => ErrorCode::Busy,
        _ => ErrorCode::Internal,
    }
}

/// The code a typed cause in `error`'s chain implies; `Internal` without one.
fn code_of(error: &anyhow::Error) -> ErrorCode {
    for cause in error.chain() {
        if cause.is::<rawtonic_onnx::RuntimeError>() {
            return ErrorCode::AiRuntimeUnavailable;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_code(io.kind());
        }
    }
    ErrorCode::Internal
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::new(io_code(error.kind()), error.to_string())
    }
}

/// The message is the error's own text (`to_string()`, without its
/// causes), as `.map_err(|e| e.to_string())` gave it.
impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::new(code_of(&error), error.to_string())
    }
}

impl From<rawtonic_onnx::RuntimeError> for CommandError {
    fn from(error: rawtonic_onnx::RuntimeError) -> Self {
        Self::new(ErrorCode::AiRuntimeUnavailable, error.to_string())
    }
}

/// An error that is only text: `Internal`.
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::internal(message)
    }
}

impl From<crate::file_management::SaveFailure> for CommandError {
    fn from(failure: crate::file_management::SaveFailure) -> Self {
        let details = match serde_json::to_value(&failure) {
            Ok(serde_json::Value::Object(mut fields)) => {
                fields.remove("message");
                Some(fields)
            }
            _ => None,
        };
        Self {
            details,
            ..Self::internal(failure.message)
        }
    }
}

/// Lets a command that still returns `String` errors call a helper that
/// returns `CommandError` with `?`: it keeps the message.
impl From<CommandError> for String {
    fn from(error: CommandError) -> Self {
        error.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_code_and_the_unchanged_message() {
        let error = CommandError::cancelled("Task cancelled");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "cancelled", "message": "Task cancelled" })
        );
        assert_eq!(error.to_string(), "Task cancelled");
        assert_eq!(String::from(error), "Task cancelled");
    }

    #[test]
    fn every_code_has_its_snake_case_name() {
        let names = [
            (ErrorCode::Cancelled, "cancelled"),
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::AlreadyExists, "already_exists"),
            (ErrorCode::PermissionDenied, "permission_denied"),
            (ErrorCode::Unsupported, "unsupported"),
            (ErrorCode::Invalid, "invalid"),
            (ErrorCode::Busy, "busy"),
            (ErrorCode::Internal, "internal"),
            (ErrorCode::Stale, "stale"),
            (ErrorCode::AiRuntimeUnavailable, "ai_runtime_unavailable"),
            (ErrorCode::ModelsMissing, "models_missing"),
            (ErrorCode::MaskParametersMissing, "mask_parameters_missing"),
            (ErrorCode::LiveStrokeResync, "live_stroke_resync"),
            (ErrorCode::RenderFailed, "render_failed"),
            (ErrorCode::GeneratedAreas, "generated_areas"),
            (ErrorCode::ExportsPending, "exports_pending"),
        ];
        for (code, name) in names {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(name));
        }
    }

    #[test]
    fn io_errors_keep_their_text_and_map_their_kind() {
        use std::io::{Error, ErrorKind};
        for (kind, code) in [
            (ErrorKind::NotFound, ErrorCode::NotFound),
            (ErrorKind::AlreadyExists, ErrorCode::AlreadyExists),
            (ErrorKind::PermissionDenied, ErrorCode::PermissionDenied),
            (ErrorKind::Unsupported, ErrorCode::Unsupported),
            (ErrorKind::InvalidInput, ErrorCode::Invalid),
            (ErrorKind::Interrupted, ErrorCode::Cancelled),
            (ErrorKind::WouldBlock, ErrorCode::Busy),
            (ErrorKind::Other, ErrorCode::Internal),
        ] {
            let io = Error::new(kind, "the text");
            let error = CommandError::from(io);
            assert_eq!((error.code, error.message.as_str()), (code, "the text"));
        }
    }

    #[test]
    fn anyhow_errors_keep_to_string_or_the_chain_and_find_a_typed_cause() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no file");
        let error = anyhow::Error::new(io).context("Reading the photo");
        assert_eq!(
            CommandError::from(anyhow::anyhow!("plain")),
            CommandError::internal("plain")
        );
        let top = CommandError::from(anyhow::Error::new(std::io::Error::other("x")).context("top"));
        assert_eq!(
            (top.code, top.message.as_str()),
            (ErrorCode::Internal, "top")
        );
        let chained = CommandError::chain(error);
        assert_eq!(chained.code, ErrorCode::NotFound);
        assert_eq!(chained.message, "Reading the photo: no file");
    }

    #[test]
    fn strings_are_internal_and_a_set_cancel_flag_makes_any_error_cancelled() {
        let error = CommandError::from("Disk full".to_string());
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.clone().or_cancelled(false).code, ErrorCode::Internal);
        let stopped = error.or_cancelled(true);
        assert_eq!(
            (stopped.code, stopped.message.as_str()),
            (ErrorCode::Cancelled, "Disk full")
        );
    }

    #[test]
    fn details_go_beside_code_and_message() {
        let error = CommandError::new(ErrorCode::GeneratedAreas, "generated-areas: B.jpg")
            .with_detail("names", json!(["B.jpg"]));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "generated_areas", "message": "generated-areas: B.jpg", "names": ["B.jpg"] })
        );
    }

    #[test]
    fn a_save_failure_keeps_its_failed_paths_beside_code_and_message() {
        let failure = crate::file_management::SaveFailure {
            message: "Could not save 1 of 2 photos: a.raf: denied".into(),
            failed: vec!["a.raf".into()],
            culling: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(CommandError::from(failure)).unwrap(),
            json!({
                "code": "internal",
                "message": "Could not save 1 of 2 photos: a.raf: denied",
                "failed": ["a.raf"]
            })
        );
    }
}

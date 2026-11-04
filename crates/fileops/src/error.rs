//! The error of this crate's public functions: a stop, and every other
//! failure with its unchanged message.

use rawtonic_runtime::Cancelled;

/// A failed or stopped file operation. The messages are the ones users see
/// (and the app's error codes depend on), so they never change.
#[derive(Debug, thiserror::Error)]
pub enum FileOpsError {
    /// Stop was pressed ("Stopped"); the applied steps were reversed.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Any other failure (a refused plan, a step that failed, a journal
    /// problem); `{:#}` shows its causes as before. A stop reported with
    /// context ("Nothing was undone: Stopped") is one of these:
    /// [`FileOpsError::is_cancelled`] finds it.
    #[error(transparent)]
    Other(anyhow::Error),
}

impl FileOpsError {
    /// Whether the operation was stopped, also when the stop is reported
    /// inside another failure's context.
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled(_) => true,
            Self::Io(_) | Self::Sqlite(_) => false,
            Self::Other(error) => error.chain().any(|cause| {
                cause.is::<Cancelled>()
                    || cause.downcast_ref::<Self>().is_some_and(Self::is_cancelled)
            }),
        }
    }
}

/// The operation's message for a stop.
pub(crate) const fn stopped() -> Cancelled {
    Cancelled::new("Stopped")
}

/// An `anyhow` error that is a `FileOpsError` or a stop (not one wrapped
/// in context) becomes that error again, so a variant survives a helper
/// returning `anyhow::Result`.
impl From<anyhow::Error> for FileOpsError {
    fn from(error: anyhow::Error) -> Self {
        let head: &(dyn std::error::Error + 'static) = &*error;
        if head.is::<Self>() {
            return error.downcast::<Self>().unwrap_or_else(Self::Other);
        }
        if let Some(cancelled) = head.downcast_ref::<Cancelled>() {
            return Self::Cancelled(*cancelled);
        }
        Self::Other(error)
    }
}

/// `anyhow::bail!` for functions returning either `anyhow::Result` or
/// `Result<_, FileOpsError>`.
macro_rules! bail {
    ($($message:tt)+) => {
        return Err(anyhow::anyhow!($($message)+).into())
    };
}

/// `anyhow::ensure!` for functions returning either `anyhow::Result` or
/// `Result<_, FileOpsError>`.
macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            $crate::error::bail!($($message)+);
        }
    };
}

pub(crate) use {bail, ensure};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stay_as_users_see_them() {
        assert_eq!(FileOpsError::from(stopped()).to_string(), "Stopped");
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(FileOpsError::from(io).to_string(), "gone");
        let sqlite = rusqlite::Error::QueryReturnedNoRows;
        assert_eq!(
            FileOpsError::from(sqlite).to_string(),
            rusqlite::Error::QueryReturnedNoRows.to_string()
        );
        let other = FileOpsError::from(anyhow::anyhow!("root").context("Moving"));
        assert_eq!(other.to_string(), "Moving");
        assert_eq!(format!("{other:#}"), "Moving: root");
        assert_eq!(format!("{:#}", anyhow::Error::from(other)), "Moving: root");
    }

    #[test]
    fn a_stop_is_found_with_or_without_context_and_keeps_its_text() {
        let plain = FileOpsError::from(anyhow::Error::new(stopped()));
        assert!(matches!(plain, FileOpsError::Cancelled(_)));
        let wrapped =
            FileOpsError::from(anyhow::Error::new(stopped()).context("Nothing was undone"));
        assert!(matches!(wrapped, FileOpsError::Other(_)) && wrapped.is_cancelled());
        assert_eq!(format!("{wrapped:#}"), "Nothing was undone: Stopped");
        assert!(!FileOpsError::from(anyhow::anyhow!("Disk full")).is_cancelled());
        let back = FileOpsError::from(anyhow::Error::from(FileOpsError::from(stopped())));
        assert!(matches!(back, FileOpsError::Cancelled(_)));
    }
}

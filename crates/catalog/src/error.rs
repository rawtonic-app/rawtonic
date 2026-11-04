//! The error of this crate's public functions: the conditions callers tell
//! apart, and every other failure with its unchanged message.

use rawtonic_metadata::MetadataError;
use rawtonic_runtime::Cancelled;

/// A failed or stopped library operation. The messages are the ones users
/// see (and the app's error codes depend on), so they never change.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// A folder scan was stopped (`SyncOptions::cancel`):
    /// "Synchronization cancelled".
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// An album, group or smart album id the library does not hold.
    #[error("Unknown album")]
    UnknownAlbum,
    /// A sidecar write refused because the photograph is offline.
    #[error("The photograph is offline; its XMP is written when it is back")]
    XmpOffline,
    /// A sidecar read or write failed; `MetadataError::XmpConflict` when
    /// it changed outside Rawtonic ([`CatalogError::is_xmp_conflict`]).
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Any other failure; `{:#}` shows its causes as before. A condition
    /// above reported with context is one of these; the `is_…` methods
    /// find it.
    #[error(transparent)]
    Other(anyhow::Error),
}

impl CatalogError {
    /// Whether the scan was stopped, also inside another error's context.
    pub fn is_cancelled(&self) -> bool {
        self.any(&|e| matches!(e, Self::Cancelled(_)), &|cause| {
            cause.is::<Cancelled>()
        })
    }

    /// Whether the photograph is offline, also inside context.
    pub fn is_xmp_offline(&self) -> bool {
        self.any(&|e| matches!(e, Self::XmpOffline), &|_| false)
    }

    /// Whether a sidecar changed outside Rawtonic, also inside context.
    pub fn is_xmp_conflict(&self) -> bool {
        self.any(
            &|e| matches!(e, Self::Metadata(MetadataError::XmpConflict)),
            &|cause| {
                cause
                    .downcast_ref::<MetadataError>()
                    .is_some_and(|m| matches!(m, MetadataError::XmpConflict))
            },
        )
    }

    fn any(
        &self,
        own: &dyn Fn(&Self) -> bool,
        cause: &dyn Fn(&(dyn std::error::Error + 'static)) -> bool,
    ) -> bool {
        if own(self) {
            return true;
        }
        let Self::Other(error) = self else {
            return false;
        };
        error
            .chain()
            .any(|c| cause(c) || c.downcast_ref::<Self>().is_some_and(|e| e.any(own, cause)))
    }
}

/// An `anyhow` error that is a `CatalogError`, a stop or a metadata error
/// (not one wrapped in context) becomes that error again, so a variant
/// survives a helper returning `anyhow::Result`.
impl From<anyhow::Error> for CatalogError {
    fn from(error: anyhow::Error) -> Self {
        let head: &(dyn std::error::Error + 'static) = &*error;
        if head.is::<Self>() {
            return error.downcast::<Self>().unwrap_or_else(Self::Other);
        }
        if let Some(cancelled) = head.downcast_ref::<Cancelled>() {
            return Self::Cancelled(*cancelled);
        }
        if head.is::<MetadataError>() {
            return error
                .downcast::<MetadataError>()
                .map_or_else(Self::Other, Self::Metadata);
        }
        Self::Other(error)
    }
}

/// Failures callers never tell apart, kept as `anyhow` did (`Other`, with
/// the same text and causes), so `?` works on them in public functions.
macro_rules! other_from {
    ($($error:ty),+ $(,)?) => {
        $(impl From<$error> for CatalogError {
            fn from(error: $error) -> Self {
                Self::Other(error.into())
            }
        })+
    };
}

other_from!(
    serde_json::Error,
    std::num::TryFromIntError,
    rawtonic_metadata::culling::FlagOutOfRange,
    crate::capture_time::TimeError,
);

/// `anyhow::bail!` for functions returning either `anyhow::Result` or
/// `Result<_, CatalogError>`.
macro_rules! bail {
    ($($message:tt)+) => {
        return Err(anyhow::anyhow!($($message)+).into())
    };
}

/// `anyhow::ensure!` for functions returning either `anyhow::Result` or
/// `Result<_, CatalogError>`.
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
        assert_eq!(
            CatalogError::from(crate::folder_sync::stopped()).to_string(),
            "Synchronization cancelled"
        );
        assert_eq!(CatalogError::UnknownAlbum.to_string(), "Unknown album");
        assert_eq!(
            CatalogError::XmpOffline.to_string(),
            "The photograph is offline; its XMP is written when it is back"
        );
        assert_eq!(
            CatalogError::from(MetadataError::XmpConflict).to_string(),
            "XMP conflict: external changes must be reviewed"
        );
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(CatalogError::from(io).to_string(), "gone");
        assert_eq!(
            CatalogError::from(rusqlite::Error::QueryReturnedNoRows).to_string(),
            rusqlite::Error::QueryReturnedNoRows.to_string()
        );
        let other = CatalogError::from(anyhow::anyhow!("root").context("Adding"));
        assert_eq!(other.to_string(), "Adding");
        assert_eq!(format!("{other:#}"), "Adding: root");
        assert_eq!(format!("{:#}", anyhow::Error::from(other)), "Adding: root");
    }

    #[test]
    fn conditions_are_found_with_or_without_context() {
        let offline = CatalogError::from(anyhow::Error::from(CatalogError::XmpOffline));
        assert!(matches!(offline, CatalogError::XmpOffline) && offline.is_xmp_offline());
        let conflict = anyhow::Error::from(MetadataError::XmpConflict).context("Existing XMP");
        let conflict = CatalogError::from(conflict);
        assert!(matches!(conflict, CatalogError::Other(_)) && conflict.is_xmp_conflict());
        assert_eq!(
            format!("{conflict:#}"),
            "Existing XMP: XMP conflict: external changes must be reviewed"
        );
        let stopped = anyhow::Error::new(crate::folder_sync::stopped()).context("Scanning");
        assert!(CatalogError::from(stopped).is_cancelled());
        let plain = CatalogError::from(anyhow::anyhow!("Disk full"));
        assert!(!plain.is_cancelled() && !plain.is_xmp_offline() && !plain.is_xmp_conflict());
    }
}

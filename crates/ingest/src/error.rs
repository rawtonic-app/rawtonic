//! The error of this crate's public functions: a stop, the library's own
//! conditions, and every other failure with its unchanged message.

use rawtonic_catalog::CatalogError;
use rawtonic_runtime::Cancelled;

/// A failed or stopped import, export or copy. The messages are the ones
/// users see (and the app's error codes depend on), so they never change.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The user stopped the work ("Ingest cancelled", "Copy cancelled").
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// The library refused or failed (`CatalogError`'s own conditions).
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Any other failure; `{:#}` shows its causes as before. A stop
    /// reported with context is one of these: [`IngestError::is_cancelled`]
    /// finds it.
    #[error(transparent)]
    Other(anyhow::Error),
}

impl IngestError {
    /// Whether the work was stopped, also inside another error's context.
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled(_) => true,
            Self::Catalog(error) => error.is_cancelled(),
            Self::Io(_) => false,
            Self::Other(error) => error.chain().any(|cause| {
                cause.is::<Cancelled>()
                    || cause.downcast_ref::<Self>().is_some_and(Self::is_cancelled)
                    || cause
                        .downcast_ref::<CatalogError>()
                        .is_some_and(CatalogError::is_cancelled)
            }),
        }
    }
}

/// An ingest stopped by the user.
pub(crate) const fn ingest_cancelled() -> Cancelled {
    Cancelled::new("Ingest cancelled")
}

/// A transfer (catalog export or import copy) stopped by the user.
pub(crate) const fn copy_cancelled() -> Cancelled {
    Cancelled::new("Copy cancelled")
}

/// An `anyhow` error that is an `IngestError`, a stop or a `CatalogError`
/// (not one wrapped in context) becomes that error again, so a variant
/// survives a helper returning `anyhow::Result`.
impl From<anyhow::Error> for IngestError {
    fn from(error: anyhow::Error) -> Self {
        let head: &(dyn std::error::Error + 'static) = &*error;
        if head.is::<Self>() {
            return error.downcast::<Self>().unwrap_or_else(Self::Other);
        }
        if let Some(cancelled) = head.downcast_ref::<Cancelled>() {
            return Self::Cancelled(*cancelled);
        }
        if head.is::<CatalogError>() {
            return error
                .downcast::<CatalogError>()
                .map_or_else(Self::Other, Self::Catalog);
        }
        Self::Other(error)
    }
}

/// Failures callers never tell apart, kept as `anyhow` did (`Other`).
macro_rules! other_from {
    ($($error:ty),+ $(,)?) => {
        $(impl From<$error> for IngestError {
            fn from(error: $error) -> Self {
                Self::Other(error.into())
            }
        })+
    };
}

other_from!(
    serde_json::Error,
    rawtonic_metadata::MetadataError,
    std::num::TryFromIntError,
    std::path::StripPrefixError,
);

/// `anyhow::bail!` for functions returning either `anyhow::Result` or
/// `Result<_, IngestError>`.
macro_rules! bail {
    ($($message:tt)+) => {
        return Err(anyhow::anyhow!($($message)+).into())
    };
}

/// `anyhow::ensure!` for functions returning either `anyhow::Result` or
/// `Result<_, IngestError>`.
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
            IngestError::from(ingest_cancelled()).to_string(),
            "Ingest cancelled"
        );
        assert_eq!(
            IngestError::from(copy_cancelled()).to_string(),
            "Copy cancelled"
        );
        assert_eq!(
            IngestError::from(CatalogError::UnknownAlbum).to_string(),
            "Unknown album"
        );
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(IngestError::from(io).to_string(), "gone");
        let other = IngestError::from(anyhow::anyhow!("root").context("Copying"));
        assert_eq!(other.to_string(), "Copying");
        assert_eq!(format!("{other:#}"), "Copying: root");
        assert_eq!(format!("{:#}", anyhow::Error::from(other)), "Copying: root");
    }

    #[test]
    fn a_stop_is_found_with_or_without_context_and_keeps_its_text() {
        let plain = IngestError::from(anyhow::Error::new(ingest_cancelled()));
        assert!(matches!(plain, IngestError::Cancelled(_)));
        let wrapped = IngestError::from(anyhow::Error::new(copy_cancelled()).context("a.raf"));
        assert!(matches!(wrapped, IngestError::Other(_)) && wrapped.is_cancelled());
        assert_eq!(format!("{wrapped:#}"), "a.raf: Copy cancelled");
        let catalog = anyhow::Error::from(CatalogError::UnknownAlbum);
        assert!(matches!(
            IngestError::from(catalog),
            IngestError::Catalog(CatalogError::UnknownAlbum)
        ));
        assert!(!IngestError::from(anyhow::anyhow!("Disk full")).is_cancelled());
    }
}

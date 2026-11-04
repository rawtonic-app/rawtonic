//! The error of this crate's public functions: the conditions callers tell
//! apart, and every other failure with its unchanged message.

/// A failed sidecar read, merge or write. The messages are the ones users
/// see (and the app's error codes depend on), so they never change.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// The sidecar changed since the library last wrote or read it (another
    /// application edited, replaced or removed it): `write_xmp` refuses to
    /// replace it until the change is reviewed.
    #[error("XMP conflict: external changes must be reviewed")]
    XmpConflict,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// An unreadable or invalid packet, a limit exceeded, or a failed
    /// publish; `{:#}` shows its causes as before.
    #[error(transparent)]
    Other(anyhow::Error),
}

/// An `anyhow` error that is a `MetadataError` (not one wrapped in context)
/// becomes that error again, so a variant survives a helper returning
/// `anyhow::Result`.
impl From<anyhow::Error> for MetadataError {
    fn from(error: anyhow::Error) -> Self {
        let head: &(dyn std::error::Error + 'static) = &*error;
        if head.is::<Self>() {
            return error.downcast::<Self>().unwrap_or_else(Self::Other);
        }
        Self::Other(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stay_as_users_see_them() {
        assert_eq!(
            MetadataError::XmpConflict.to_string(),
            "XMP conflict: external changes must be reviewed"
        );
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no sidecar");
        assert_eq!(MetadataError::from(io).to_string(), "no sidecar");
        let other = anyhow::anyhow!("root").context("Reading the sidecar");
        let other = MetadataError::from(other);
        assert_eq!(other.to_string(), "Reading the sidecar");
        assert_eq!(format!("{other:#}"), "Reading the sidecar: root");
        assert_eq!(
            format!("{:#}", anyhow::Error::from(other)),
            "Reading the sidecar: root"
        );
    }

    #[test]
    fn a_variant_survives_a_trip_through_anyhow_but_context_keeps_its_text() {
        let back = MetadataError::from(anyhow::Error::from(MetadataError::XmpConflict));
        assert!(matches!(back, MetadataError::XmpConflict));
        let wrapped = anyhow::Error::from(MetadataError::XmpConflict).context("Saving");
        let wrapped = MetadataError::from(wrapped);
        assert!(matches!(wrapped, MetadataError::Other(_)));
        assert_eq!(
            format!("{wrapped:#}"),
            "Saving: XMP conflict: external changes must be reviewed"
        );
    }
}

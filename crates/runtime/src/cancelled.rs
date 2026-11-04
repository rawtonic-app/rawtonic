//! The one error for work stopped on request (the user's Stop, or a newer
//! request for the same thing). Core and engine crates report a stop with
//! it, so callers tell a stop from a failure by its type, never by text.

use std::fmt;

/// Work stopped because it was asked to. It carries the message the
/// stopped operation has always shown ("Stopped", "Ingest cancelled", …),
/// so users see the same text whichever crate stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled {
    message: &'static str,
}

impl Cancelled {
    pub const fn new(message: &'static str) -> Self {
        Self { message }
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_the_message_it_was_given() {
        let stopped = Cancelled::new("Stopped");
        assert_eq!(stopped.to_string(), "Stopped");
        assert_eq!(stopped.message(), "Stopped");
    }
}

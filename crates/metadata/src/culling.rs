//! A photo's rating and pick flag as values that are valid by construction.
//!
//! Stored as before (`rating` 0–5 as an integer, `flag` -1/0/1, in sidecars,
//! the catalog and the editor state): these types check and name the
//! values; the stored forms are their [`Rating::stars`] and [`Flag::value`].

use std::fmt;

/// A star rating, 0 (unrated) to [`Rating::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Rating(u8);

impl Rating {
    pub const MAX: u8 = 5;
    pub const UNRATED: Rating = Rating(0);

    /// `None` for more than [`Rating::MAX`] stars.
    pub const fn new(stars: u8) -> Option<Self> {
        if stars <= Self::MAX {
            Some(Self(stars))
        } else {
            None
        }
    }

    /// Any stored integer; `None` outside 0–5.
    pub fn from_stored(stars: impl TryInto<u8>) -> Option<Self> {
        stars.try_into().ok().and_then(Self::new)
    }

    /// At most [`Rating::MAX`] stars (for values read leniently).
    pub const fn clamped(stars: u8) -> Self {
        Self(if stars > Self::MAX { Self::MAX } else { stars })
    }

    pub const fn stars(self) -> u8 {
        self.0
    }

    pub const fn is_rated(self) -> bool {
        self.0 > 0
    }
}

impl TryFrom<u8> for Rating {
    type Error = RatingOutOfRange;

    fn try_from(stars: u8) -> Result<Self, Self::Error> {
        Self::new(stars).ok_or(RatingOutOfRange)
    }
}

impl From<Rating> for u8 {
    fn from(rating: Rating) -> u8 {
        rating.0
    }
}

/// A rating above five stars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatingOutOfRange;

impl fmt::Display for RatingOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rating must be 0–5")
    }
}

impl std::error::Error for RatingOutOfRange {}

/// The pick flag: rejected, none, or picked (stored -1, 0, 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Flag {
    Reject,
    #[default]
    Unflagged,
    Pick,
}

impl Flag {
    /// `None` for a stored value other than -1, 0 or 1.
    pub fn from_stored(value: impl TryInto<i8>) -> Option<Self> {
        match value.try_into().ok()? {
            -1 => Some(Self::Reject),
            0 => Some(Self::Unflagged),
            1 => Some(Self::Pick),
            _ => None,
        }
    }

    /// The stored value.
    pub const fn value(self) -> i8 {
        match self {
            Self::Reject => -1,
            Self::Unflagged => 0,
            Self::Pick => 1,
        }
    }
}

impl TryFrom<i8> for Flag {
    type Error = FlagOutOfRange;

    fn try_from(value: i8) -> Result<Self, Self::Error> {
        Self::from_stored(value).ok_or(FlagOutOfRange)
    }
}

impl From<Flag> for i8 {
    fn from(flag: Flag) -> i8 {
        flag.value()
    }
}

/// A flag other than -1, 0 or 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagOutOfRange;

impl fmt::Display for FlagOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Flag must be -1, 0 or 1")
    }
}

impl std::error::Error for FlagOutOfRange {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratings_are_zero_to_five_stars() {
        for stars in 0..=5u8 {
            assert_eq!(Rating::new(stars).map(Rating::stars), Some(stars));
            assert_eq!(u8::from(Rating::try_from(stars).unwrap()), stars);
        }
        assert_eq!(Rating::new(6), None);
        assert_eq!(Rating::try_from(255), Err(RatingOutOfRange));
        assert_eq!(Rating::from_stored(5u64), Rating::new(5));
        assert_eq!(Rating::from_stored(6u64), None);
        assert_eq!(Rating::from_stored(-1i64), None);
        assert_eq!(Rating::clamped(9).stars(), 5);
        assert_eq!(Rating::clamped(3).stars(), 3);
        assert!(!Rating::UNRATED.is_rated());
        assert_eq!(RatingOutOfRange.to_string(), "Rating must be 0–5");
    }

    #[test]
    fn flags_are_stored_as_minus_one_zero_one() {
        for flag in [Flag::Reject, Flag::Unflagged, Flag::Pick] {
            assert_eq!(Flag::from_stored(flag.value()), Some(flag));
            assert_eq!(Flag::try_from(i8::from(flag)), Ok(flag));
        }
        assert_eq!(Flag::Reject.value(), -1);
        assert_eq!(Flag::Pick.value(), 1);
        assert_eq!(Flag::from_stored(2i8), None);
        assert_eq!(Flag::from_stored(-2i64), None);
        assert_eq!(Flag::from_stored(300i64), None);
        assert_eq!(Flag::default(), Flag::Unflagged);
        assert_eq!(FlagOutOfRange.to_string(), "Flag must be -1, 0 or 1");
    }
}

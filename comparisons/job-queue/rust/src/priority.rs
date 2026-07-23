//! Job priority — a validated `u8` in the closed range `[1, 10]`.
//!
//! Wrapping the raw byte in a newtype keeps invalid values from ever reaching
//! the queue's internals: every code path that consumes a `Priority` can
//! trust the bounds without re-checking. Higher numbers are higher priority
//! (10 acquires before 1).

use serde::{Deserialize, Serialize};

/// Validated job priority. Values outside `[1, 10]` cannot be constructed.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Serialize, Deserialize)]
pub struct Priority(u8);

impl Priority {
    pub const MIN: Priority = Priority(1);
    pub const MAX: Priority = Priority(10);
    pub const DEFAULT: Priority = Priority(5);

    /// Construct a priority, validating that `value` is in `[1, 10]`.
    pub fn new(value: u8) -> Result<Self, InvalidPriority> {
        if (Self::MIN.0..=Self::MAX.0).contains(&value) {
            Ok(Priority(value))
        } else {
            Err(InvalidPriority(value))
        }
    }

    /// Raw numeric value, guaranteed to be in `[1, 10]`.
    pub fn value(self) -> u8 {
        self.0
    }
}

impl Default for Priority {
    fn default() -> Self {
        Priority::DEFAULT
    }
}

/// Public form of [`OutOfRange`]; returned by [`Priority::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPriority(pub u8);

impl std::fmt::Display for InvalidPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "priority {} is out of range [1, 10]", self.0)
    }
}

impl std::error::Error for InvalidPriority {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero() {
        assert!(Priority::new(0).is_err());
    }

    #[test]
    fn rejects_eleven() {
        assert!(Priority::new(11).is_err());
    }

    #[test]
    fn accepts_endpoints() {
        assert_eq!(Priority::new(1).unwrap().value(), 1);
        assert_eq!(Priority::new(10).unwrap().value(), 10);
    }

    #[test]
    fn default_is_five() {
        assert_eq!(Priority::default().value(), 5);
    }

    #[test]
    fn ord_higher_value_sorts_higher() {
        assert!(Priority::new(9).unwrap() > Priority::new(1).unwrap());
    }
}

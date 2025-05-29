//! Contains time related utilities.

use std::time::SystemTime;

/// Trait that provides current time since UNIX epoch.
pub trait TimeSource {
    /// Provides current time since UNIX epoch as nanos.
    fn current_time_nanos(&self) -> u64;
}

/// Uses `SystemTime` as [`TimeSource`].
pub struct SystemTimeClockSource;

impl TimeSource for SystemTimeClockSource {
    #[inline]
    fn current_time_nanos(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}

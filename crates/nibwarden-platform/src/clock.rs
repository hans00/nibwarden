//! A monotonic clock that keeps counting while the machine sleeps, for idle timeouts: a
//! laptop that slept past the timeout must lock on the next tick.

use std::time::Duration;

/// Time since an arbitrary fixed point, including time spent suspended.
pub fn since_boot() -> Duration {
    imp::since_boot()
}

#[cfg_attr(target_os = "linux", path = "linux/clock.rs")]
#[cfg_attr(target_os = "macos", path = "macos/clock.rs")]
#[cfg_attr(windows, path = "windows/clock.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/clock.rs"
)]
mod imp;

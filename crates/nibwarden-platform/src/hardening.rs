//! Keeps key material out of core dumps and away from debuggers (phase1.md §5.4).

/// Called first thing in the daemon. Problems are printed to stderr: logging is not set
/// up yet, and none of them is fatal.
pub fn harden_process() {
    imp::harden_process();
}

#[cfg_attr(target_os = "linux", path = "linux/hardening.rs")]
#[cfg_attr(target_os = "macos", path = "macos/hardening.rs")]
#[cfg_attr(windows, path = "windows/hardening.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/hardening.rs"
)]
mod imp;

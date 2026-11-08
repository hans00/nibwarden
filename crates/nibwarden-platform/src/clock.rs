//! A monotonic clock that keeps counting while the machine sleeps, for idle timeouts: a
//! laptop that slept past the timeout must lock on the next tick.

use std::time::Duration;

/// Time since an arbitrary fixed point, including time spent suspended.
pub fn since_boot() -> Duration {
    imp::since_boot()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::time::Duration;

    /// `CLOCK_BOOTTIME`: unlike `Instant` (CLOCK_MONOTONIC) it includes suspend.
    pub fn since_boot() -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime only writes to `ts`; CLOCK_BOOTTIME always exists on Linux.
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::{sync::OnceLock, time::Duration, time::Instant};

    /// Placeholder until ported (macOS: `mach_continuous_time`, Windows:
    /// `QueryInterruptTime`). `Instant` may stop during sleep.
    pub fn since_boot() -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }
}

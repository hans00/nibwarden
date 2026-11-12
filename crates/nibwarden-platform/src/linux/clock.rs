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

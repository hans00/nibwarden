use std::{sync::OnceLock, time::Duration, time::Instant};

/// Placeholder until ported (macOS: `mach_continuous_time`, Windows:
/// `QueryInterruptTime`). `Instant` may stop during sleep.
pub fn since_boot() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

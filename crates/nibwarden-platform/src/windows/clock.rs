use std::time::Duration;

use windows::Win32::System::{
    SystemInformation::GetTickCount64, WindowsProgramming::QueryInterruptTime,
};

/// The larger of `QueryInterruptTime` and `GetTickCount64`, both of which count time spent
/// asleep (as opposed to `QueryUnbiasedInterruptTime`, which stops in sleep and
/// hibernation). Two sources, so that the clock keeps counting through sleep even if one
/// of them turns out not to; both are monotonic, so their maximum is too.
pub fn since_boot() -> Duration {
    // SAFETY: plain queries without arguments.
    let (interrupt, ticks) = unsafe { (QueryInterruptTime(), GetTickCount64()) };
    // Interrupt time is in units of 100 ns.
    let interrupt = Duration::from_nanos(interrupt.saturating_mul(100));
    interrupt.max(Duration::from_millis(ticks))
}

#[cfg(all(test, windows))]
mod windows_tests {
    #[test]
    fn counts_up_from_boot() {
        let a = super::since_boot();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let b = super::since_boot();
        assert!(b >= a);
        assert!(b > std::time::Duration::from_secs(1));
    }
}

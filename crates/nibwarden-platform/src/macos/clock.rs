use std::time::Duration;

/// `mach_timebase_info_data_t`; libc's copy is deprecated.
#[repr(C)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut TimebaseInfo) -> libc::c_int;

    /// `<mach/mach_time.h>`: "the current value of a clock that increments monotonically
    /// in tick units ... even while the system is asleep" (unlike `mach_absolute_time`,
    /// which stops, and with it `Instant`).
    fn mach_continuous_time() -> u64;
}

/// `mach_continuous_time` converted from ticks to time with `mach_timebase_info`.
pub fn since_boot() -> Duration {
    let mut base = TimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: mach_timebase_info only writes to `base`; mach_continuous_time has no
    // arguments and no failure mode.
    let ticks = unsafe {
        if mach_timebase_info(&mut base) != 0 || base.denom == 0 {
            // Cannot happen; 1/1 is what Intel Macs report.
            base = TimebaseInfo { numer: 1, denom: 1 };
        }
        mach_continuous_time()
    };
    ticks_to_duration(ticks, base.numer, base.denom)
}

fn ticks_to_duration(ticks: u64, numer: u32, denom: u32) -> Duration {
    let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_ticks() {
        // Apple silicon: 125/3 ns per tick (24 MHz).
        assert_eq!(
            ticks_to_duration(24_000_000, 125, 3),
            Duration::from_secs(1)
        );
        assert_eq!(ticks_to_duration(5, 1, 1), Duration::from_nanos(5));
    }

    #[test]
    fn counts_up() {
        let a = since_boot();
        let b = since_boot();
        assert!(a > Duration::ZERO && b >= a);
    }
}

//! Keeps key material out of core dumps and away from debuggers (phase1.md §5.4).

/// Called first thing in the daemon. Problems are printed to stderr: logging is not set
/// up yet, and none of them is fatal.
pub fn harden_process() {
    imp::harden_process();
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn harden_process() {
        // SAFETY: plain syscalls without pointers into Rust memory, except `limit`, which
        // outlives the call.
        unsafe {
            // Files we create (SDK database included) are private regardless of the
            // caller's umask.
            libc::umask(0o077);
            // Also blocks same-uid ptrace where Yama is off.
            if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                eprintln!("warning: PR_SET_DUMPABLE failed");
            }
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
                eprintln!("warning: could not disable core dumps");
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn harden_process() {
        eprintln!("warning: process hardening is not implemented on this platform");
    }
}

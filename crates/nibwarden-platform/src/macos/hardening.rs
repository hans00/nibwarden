pub fn harden_process() {
    // SAFETY: plain syscalls without pointers into Rust memory, except `limit`, which
    // outlives the call.
    unsafe {
        // Files we create (SDK database included) are private regardless of the
        // caller's umask.
        libc::umask(0o077);
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
            eprintln!("warning: could not disable core dumps");
        }
        // Refuse debuggers (lldb, dtrace) that would attach later: an attempt ends in a
        // segmentation violation in the tracer. A debugger that is already attached makes
        // the process exit with ENOTSUP (ptrace(2)), so debug builds, which are the ones
        // run under lldb, skip it. Root and holders of the task-port entitlements still
        // reach the process; this raises the bar for same-user tools, like
        // PR_SET_DUMPABLE on Linux.
        if !cfg!(debug_assertions)
            && libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) != 0
        {
            eprintln!("warning: PT_DENY_ATTACH failed");
        }
    }
}

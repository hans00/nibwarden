use std::{
    io,
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{launchd, macos_logic::plist_run_at_load};

/// `me.hans00.nibwarden.daemon` (`launchd::LABEL_PREFIX` and `daemon`).
const LABEL: &str = "me.hans00.nibwarden.daemon";

/// `nibwardend` next to the running program, else on `PATH`.
fn daemon_exe() -> io::Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(sibling) = exe.parent().map(|dir| dir.join("nibwardend"))
        && sibling.is_file()
    {
        return Ok(sibling);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("nibwardend"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nibwardend was not found"))
}

fn starts_at_login_now() -> bool {
    launchd::plist_path(LABEL)
        .and_then(std::fs::read_to_string)
        .is_ok_and(|text| plist_run_at_load(&text))
}

/// Writes the agent's property list for the daemon that is next to us; whether it changed.
fn write_agent(run_at_load: bool) -> io::Result<bool> {
    let program = daemon_exe()?;
    launchd::write_agent(
        LABEL,
        &[program.to_string_lossy().into_owned()],
        run_at_load,
        launchd::log_path("nibwardend").as_deref(),
    )
}

/// `RunAtLoad` in `~/Library/LaunchAgents/me.hans00.nibwarden.daemon.plist`: launchd loads
/// the agent at the next login, and starts it then if this is on. (A daemon running now is
/// left alone either way.)
pub fn set_start_at_login(on: bool) -> io::Result<()> {
    if !on && launchd::plist_path(LABEL).is_ok_and(|p| !p.exists()) {
        return Ok(());
    }
    write_agent(on).map(drop)
}

pub fn starts_at_login() -> bool {
    starts_at_login_now()
}

/// `launchctl bootstrap gui/<uid> <plist>` then `launchctl kickstart gui/<uid>/<label>`
/// (which leaves a running instance alone). Outside the GUI session (an SSH login, where
/// there is no `gui/<uid>` domain) `nibwardend` is started directly instead, detached.
pub async fn start_daemon() -> bool {
    let started = match write_agent(starts_at_login_now()) {
        Ok(changed) => start_with_launchd(changed).await,
        Err(e) => {
            tracing::debug!(error = %e, "cannot write the launchd agent");
            false
        }
    };
    started || spawn_directly()
}

async fn start_with_launchd(plist_changed: bool) -> bool {
    let Ok(plist) = launchd::plist_path(LABEL) else {
        return false;
    };
    let domain = launchd::domain();
    let target = format!("{domain}/{LABEL}");
    if plist_changed {
        // A loaded job keeps the property list it was loaded with; unload it so the new
        // one applies. (Nothing runs: we are starting the daemon because it is not up.)
        launchd::launchctl(&["bootout", &target]).await;
    }
    let plist = plist.to_string_lossy().into_owned();
    // Fails if the agent is loaded already, which is fine; whether it is loaded is
    // what `print` tells.
    let loaded = launchd::launchctl(&["bootstrap", &domain, &plist]).await
        || launchd::launchctl(&["print", &target]).await;
    loaded && launchd::launchctl(&["kickstart", &target]).await
}

/// The fallback without a launchd domain to start it in.
fn spawn_directly() -> bool {
    let Ok(program) = daemon_exe() else {
        return false;
    };
    match Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: a Ctrl-C in the caller's terminal must not stop it.
        .process_group(0)
        .spawn()
    {
        Ok(mut child) => {
            // Reap it if it exits while we are still around.
            std::thread::spawn(move || child.wait());
            true
        }
        Err(e) => {
            tracing::debug!(error = %e, "cannot start nibwardend");
            false
        }
    }
}

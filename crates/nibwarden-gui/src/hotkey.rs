//! `nibwarden-gui shortcut` (macOS, Windows): holds the quick access shortcut, which there
//! no desktop setting can run, and opens quick access when it is pressed. Started when
//! the shortcut is set and at every login; ends when the shortcut is cleared (it checks
//! every few seconds, and follows a changed binding the same way).

use std::{cell::RefCell, path::PathBuf, process::ExitCode, rc::Rc, time::Duration};

use nibwarden_platform::{autostart, paths, shortcut};

/// The autostart entry's name.
const AUTOSTART: &str = "nibwarden-shortcut";
/// How often the holder checks whether the binding changed or was cleared.
const REFRESH: Duration = Duration::from_secs(3);

/// Holds the shortcut now and at every login (`on`), or not at login any more (the
/// running holder ends by itself once the binding is gone). Nothing where the desktop
/// runs the shortcut itself (Linux).
pub fn set(on: bool) -> std::io::Result<()> {
    if !shortcut::needs_holder() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let command = [exe.display().to_string(), "shortcut".to_owned()];
    autostart::set(AUTOSTART, "nibwarden", &command, on)?;
    if on {
        spawn(&exe, "shortcut")?;
    }
    Ok(())
}

/// With a shortcut set, brings its holder back if it was quit, and points the login entry
/// at this program. Called whenever a nibwarden window opens.
pub fn restore() {
    if shortcut::needs_holder()
        && shortcut::current().is_some()
        && let Err(e) = set(true)
    {
        tracing::warn!(error = %e, "cannot hold the quick access shortcut");
    }
}

fn spawn(exe: &PathBuf, mode: &str) -> std::io::Result<()> {
    let mut child = std::process::Command::new(exe)
        .arg(mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub fn run() -> ExitCode {
    let Some(binding) = shortcut::current() else {
        return ExitCode::SUCCESS;
    };
    // One holder; starting it again while it runs does nothing.
    let lock = paths::runtime_dir().and_then(|dir| {
        nibwarden_platform::fs::create_private_dir(&dir).ok()?;
        nibwarden_platform::fs::try_lock(&dir.join("shortcut.lock")).ok()
    });
    let Some(Some(_lock)) = lock else {
        return ExitCode::SUCCESS;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = crate::windowless::select() {
        eprintln!("nibwarden-gui: {e}");
        return ExitCode::FAILURE;
    }
    let hold = move |binding: &str| {
        let exe = exe.clone();
        shortcut::hold(binding, move || {
            if let Err(e) = spawn(&exe, "quick") {
                tracing::warn!(error = %e, "cannot open quick access");
            }
        })
    };
    // On this thread: the shortcut's events come through the windowless loop's wait.
    let held = match hold(&binding) {
        Ok(held) => Rc::new(RefCell::new(Some((binding, held)))),
        Err(e) => {
            eprintln!("nibwarden-gui: cannot hold the shortcut {binding}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, REFRESH, {
        let held = held.clone();
        move || {
            let now = shortcut::current();
            let unchanged = held.borrow().as_ref().map(|(b, _)| b) == now.as_ref();
            if unchanged {
                return;
            }
            // Let go of the old one first: the new binding may share keys with it.
            held.borrow_mut().take();
            let Some(binding) = now else {
                let _ = slint::quit_event_loop();
                return;
            };
            match hold(&binding) {
                Ok(new) => *held.borrow_mut() = Some((binding, new)),
                Err(e) => {
                    eprintln!("nibwarden-gui: cannot hold the shortcut {binding}: {e}");
                    let _ = slint::quit_event_loop();
                }
            }
        }
    });
    let result = slint::run_event_loop_until_quit();
    drop(timer);
    held.borrow_mut().take();
    // macOS and Windows let go of it through this thread's loop.
    nibwarden_platform::main_thread::wait(Some(Duration::ZERO));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

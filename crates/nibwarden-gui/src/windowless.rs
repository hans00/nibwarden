//! A Slint platform without windows, for the tray icon: it needs Slint only for the
//! translated texts (`TrayText`), its timers and its event loop. winit would make a
//! native window for every component, shown or not, and on Wayland that hidden toplevel
//! (app id `nibwarden`) keeps the app in the dock. The loop waits in the platform's
//! `main_thread`, which also runs the tray icon's native events on macOS and Windows.

use std::{
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use nibwarden_platform::main_thread;

use slint::platform::{
    EventLoopProxy, Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};

enum Event {
    Run(Box<dyn FnOnce() + Send>),
    Quit,
}

struct Windowless {
    started: Instant,
    events: mpsc::Receiver<Event>,
    sender: mpsc::Sender<Event>,
}

struct Proxy(mpsc::Sender<Event>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Quit)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)?;
        main_thread::wake();
        Ok(())
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Run(event))
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)?;
        main_thread::wake();
        Ok(())
    }
}

impl Platform for Windowless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.sender.clone())))
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        loop {
            slint::platform::update_timers_and_animations();
            loop {
                match self.events.try_recv() {
                    Ok(Event::Run(run)) => run(),
                    Ok(Event::Quit) => return Ok(()),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            // An event sent from now on wakes the wait, or finds it already woken.
            main_thread::wait(slint::platform::duration_until_next_timer_update());
        }
    }
}

/// Makes this process's Slint platform the windowless one; before any component exists.
pub fn select() -> Result<(), slint::PlatformError> {
    let (sender, events) = mpsc::channel();
    slint::platform::set_platform(Box::new(Windowless {
        started: Instant::now(),
        events,
        sender,
    }))
    .map_err(|e| slint::PlatformError::Other(e.to_string()))
}

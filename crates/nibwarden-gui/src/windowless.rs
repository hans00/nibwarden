//! A Slint platform without windows, for the tray icon: it needs Slint only for the
//! translated texts (`TrayText`), its timers and its event loop. winit would make a
//! native window for every component, shown or not, and on Wayland that hidden toplevel
//! (app id `nibwarden`) keeps the app in the dock.

use std::{
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

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
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Run(event))
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
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
            let event = match slint::platform::duration_until_next_timer_update() {
                Some(wait) => match self.events.recv_timeout(wait) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                },
                None => match self.events.recv() {
                    Ok(event) => event,
                    Err(_) => return Ok(()),
                },
            };
            match event {
                Event::Run(run) => run(),
                Event::Quit => return Ok(()),
            }
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

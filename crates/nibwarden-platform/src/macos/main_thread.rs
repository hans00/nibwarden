use std::{sync::Once, time::Duration};

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint};

/// `subtype` of the application-defined event [`wake`] posts.
const WAKE: i16 = 0x4e57;

pub fn wait(timeout: Option<Duration>) {
    let mtm = MainThreadMarker::new().expect("the tray's loop runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        // A menu bar item only: no Dock icon, no menu bar of its own.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
    });
    let mut until = match timeout {
        Some(timeout) => NSDate::dateWithTimeIntervalSinceNow(timeout.as_secs_f64()),
        None => NSDate::distantFuture(),
    };
    loop {
        // SAFETY: the default run loop mode is a constant AppKit defines.
        let mode = unsafe { NSDefaultRunLoopMode };
        let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            mode,
            true,
        ) else {
            break;
        };
        let ours = event.r#type() == NSEventType::ApplicationDefined && event.subtype().0 == WAKE;
        if !ours {
            app.sendEvent(&event);
        }
        // Handle what else is queued, without waiting again.
        until = NSDate::distantPast();
    }
}

pub fn wake() {
    // The main queue runs inside the event loop's waiting; the event it posts there ends
    // the wait (AppKit wants events posted from the main thread).
    DispatchQueue::main().exec_async(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            WAKE,
            0,
            0,
        );
        if let Some(event) = event {
            NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
        }
    });
}

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

static WOKEN: Mutex<bool> = Mutex::new(false);
static CHANGED: Condvar = Condvar::new();

pub fn wait(timeout: Option<Duration>) {
    let mut woken = WOKEN.lock().expect("poisoned");
    if !*woken {
        woken = match timeout {
            Some(timeout) => CHANGED.wait_timeout(woken, timeout).expect("poisoned").0,
            None => CHANGED.wait(woken).expect("poisoned"),
        };
    }
    *woken = false;
}

pub fn wake() {
    *WOKEN.lock().expect("poisoned") = true;
    CHANGED.notify_all();
}

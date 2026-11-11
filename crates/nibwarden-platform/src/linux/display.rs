/// A Wayland or X11 display in our environment. A systemd user unit gets these from
/// the graphical session (GNOME and KDE import them into the user manager).
pub fn available() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
}

type GetDisplay = unsafe extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
type Initialize = unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32, *mut i32) -> u32;
type Terminate = unsafe extern "C" fn(*mut std::ffi::c_void) -> u32;

/// EGL initializes its default display. libglvnd's libEGL without a vendor driver
/// (a container, NixOS without `hardware.graphics`) fails here.
pub fn opengl_available() -> bool {
    // SAFETY: EGL 1.4 entry points called with their C signatures; the display is
    // terminated again. The library stays loaded: the renderer loads it next, and
    // unloading a GL driver is not safe.
    unsafe {
        let Ok(egl) = libloading::Library::new("libEGL.so.1") else {
            return false;
        };
        let egl = std::mem::ManuallyDrop::new(egl);
        let (Ok(get_display), Ok(initialize), Ok(terminate)) = (
            egl.get::<GetDisplay>(b"eglGetDisplay\0"),
            egl.get::<Initialize>(b"eglInitialize\0"),
            egl.get::<Terminate>(b"eglTerminate\0"),
        ) else {
            return false;
        };
        // EGL_DEFAULT_DISPLAY
        let display = get_display(std::ptr::null_mut());
        if display.is_null() {
            return false;
        }
        let (mut major, mut minor) = (0, 0);
        let ready = initialize(display, &mut major, &mut minor) != 0;
        if ready {
            terminate(display);
        }
        ready
    }
}

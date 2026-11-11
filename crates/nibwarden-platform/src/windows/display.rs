use windows::{
    Win32::{
        Foundation::HANDLE,
        Graphics::{
            Gdi::{GetDC, ReleaseDC},
            OpenGL::{
                ChoosePixelFormat, GL_VERSION, HGLRC, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW,
                PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR, SetPixelFormat,
                glGetString, wglCreateContext, wglDeleteContext, wglMakeCurrent,
            },
        },
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            StationsAndDesktops::{
                GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
            },
            Threading::GetCurrentProcessId,
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP},
    },
    core::w,
};

/// `WSF_VISIBLE`: the window station is the one connected to the user's display. Services
/// and other non-interactive logons (an SSH session, a scheduled task that runs whether or
/// not the user is logged on) get one that is not.
const WSF_VISIBLE: u32 = 0x0001;

/// An interactive desktop: not session 0 (where services live) and a visible window
/// station.
pub fn available() -> bool {
    // SAFETY: queries about this process; the flags structure is the size told.
    unsafe {
        let mut session = 0u32;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut session).is_err() || session == 0 {
            return false;
        }
        let Ok(station) = GetProcessWindowStation() else {
            return false;
        };
        let mut flags = USEROBJECTFLAGS::default();
        let mut len = 0u32;
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_FLAGS,
            Some((&raw mut flags).cast()),
            size_of::<USEROBJECTFLAGS>() as u32,
            Some(&mut len),
        )
        .is_ok()
            && flags.dwFlags & WSF_VISIBLE != 0
    }
}

/// A window's OpenGL context can be made current and is better than the 1.1 of "GDI
/// Generic", the software implementation Windows falls back to where there is no driver
/// (a Remote Desktop session, a virtual machine without 3D): the renderer fails there only
/// when its window opens, which leaves no window at all. Asked before choosing a renderer.
pub fn opengl_available() -> bool {
    // SAFETY: a hidden window of a system class, with a device context and a GL context
    // that are all released again; the driver stays loaded, which is fine for a GL driver
    // the renderer loads next.
    unsafe {
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        ) else {
            return false;
        };
        let dc = GetDC(Some(window));
        let version = (|| {
            let format = PIXELFORMATDESCRIPTOR {
                nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16,
                nVersion: 1,
                dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
                iPixelType: PFD_TYPE_RGBA,
                cColorBits: 32,
                cDepthBits: 24,
                ..Default::default()
            };
            let index = ChoosePixelFormat(dc, &format);
            if index == 0 || SetPixelFormat(dc, index, &format).is_err() {
                return None;
            }
            let context = wglCreateContext(dc).ok()?;
            let version = if wglMakeCurrent(dc, context).is_ok() {
                let text = glGetString(GL_VERSION);
                let version = (!text.is_null()).then(|| {
                    std::ffi::CStr::from_ptr(text.cast())
                        .to_string_lossy()
                        .into_owned()
                });
                let _ = wglMakeCurrent(dc, HGLRC::default());
                version
            } else {
                None
            };
            let _ = wglDeleteContext(context);
            version
        })();
        ReleaseDC(Some(window), dc);
        let _ = DestroyWindow(window);
        version.is_some_and(|v| at_least_2(&v))
    }
}

/// `GL_VERSION` starts with `major.minor`.
fn at_least_2(version: &str) -> bool {
    version
        .split(['.', ' '])
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_gl_versions() {
        assert!(!super::at_least_2("1.1.0"));
        assert!(super::at_least_2("2.1 Mesa"));
        assert!(super::at_least_2("4.6.0 NVIDIA 555"));
        assert!(!super::at_least_2("Mesa"));
    }

    #[test]
    fn probes_without_crashing() {
        // CI runners and Wine may have no GL; only that asking does not crash.
        let _ = super::opengl_available();
        let _ = super::available();
    }
}

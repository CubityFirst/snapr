//! The name snapr's windows go by, so the desktop can tell they're snapr's:
//! `snapr.desktop`'s icon and permissions, and compositors' per-app rules
//! (say, no animation for the capture overlay).

use winit::window::{Window, WindowAttributes};

/// The app id on Wayland, and the window class on X11.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const APP_ID: &str = "snapr";

/// Attributes for a new window, named as snapr's.
pub fn window_attributes() -> WindowAttributes {
    let attrs = Window::default_attributes();
    // The same name for both: winit uses it for whichever it runs on.
    #[cfg(target_os = "linux")]
    let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, APP_ID, APP_ID);
    attrs
}

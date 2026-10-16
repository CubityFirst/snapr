//! macOS specifics: the Screen Recording permission, the capture overlay's
//! window level, and a Dock icon while the main window is open.

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSView, NSWindowCollectionBehavior,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// What to do when snapr can't see the screen.
pub const SCREEN_RECORDING_OFF: &str = "snapr can't see the screen: turn it on under System \
    Settings → Privacy & Security → Screen & System Audio Recording, then restart snapr";

/// Whether snapr may capture the screen. Without the permission macOS hands
/// back blank pictures rather than an error.
pub fn screen_recording_allowed() -> bool {
    // SAFETY: no arguments; safe from any thread.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Has macOS ask for the Screen Recording permission if snapr doesn't have
/// it. macOS only asks the first time; after that it lists snapr, off, in
/// System Settings.
pub fn ask_for_screen_recording() {
    if !screen_recording_allowed() {
        // SAFETY: as above.
        unsafe { CGRequestScreenCaptureAccess() };
    }
}

/// Puts the capture overlay above the menu bar and Dock (the floating level
/// winit's always-on-top gives is below them), on whichever Space is
/// showing, full-screen apps' included.
pub fn cover_everything(window: &Window) {
    /// NSScreenSaverWindowLevel.
    const LEVEL: isize = 1000;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    // SAFETY: winit's view for a window it just made on this (the main)
    // thread, alive while `window` is.
    unsafe {
        let view: &NSView = h.ns_view.cast().as_ref();
        if let Some(ns_window) = view.window() {
            ns_window.setLevel(LEVEL);
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary,
            );
        }
    }
}

/// Shows snapr in the Dock and the app switcher (while the main window is
/// open), or keeps it to the menu bar.
pub fn show_in_dock(show: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if show {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    app.setActivationPolicy(policy);
    if show {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

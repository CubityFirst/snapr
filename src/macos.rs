//! macOS specifics: the Screen Recording permission, the capture overlay's
//! window level and the notch, and a Dock icon while the main window is
//! open.

use std::collections::HashSet;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSView, NSWindow, NSWindowCollectionBehavior,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *mut c_void;
}

/// The ids of the ordinary app windows on screen, for snapping to. macOS
/// also lists the menu bar, the Dock and other apps' invisible full-screen
/// windows (window managers, menu bar tools), which sit on higher layers or
/// are fully transparent; snapping found those first, everywhere.
pub fn normal_windows() -> HashSet<u32> {
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
    // SAFETY: a Copy function, so the CFArray (an NSArray of NSDictionary,
    // toll-free bridged) is ours to release.
    let windows = unsafe {
        Retained::from_raw(
            CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP_ELEMENTS, 0)
                .cast::<NSArray<NSDictionary<NSString, AnyObject>>>(),
        )
    };
    let Some(windows) = windows else {
        return HashSet::new();
    };
    let number = |window: &NSDictionary<NSString, AnyObject>, key: &str| {
        window
            .objectForKey(&NSString::from_str(key))
            .and_then(|v| v.downcast::<NSNumber>().ok())
    };
    windows
        .iter()
        .filter(|w| number(w, "kCGWindowLayer").is_some_and(|l| l.intValue() == 0))
        .filter(|w| number(w, "kCGWindowAlpha").is_none_or(|a| a.doubleValue() > 0.0))
        .filter_map(|w| number(&w, "kCGWindowNumber").map(|n| n.unsignedIntValue()))
        .collect()
}

/// What to do when snapr can't see the screen. (Paths are written with `>`,
/// as Apple does: egui's font has no arrows.)
pub const SCREEN_RECORDING_OFF: &str = "snapr can't see the screen: turn it on under System \
    Settings > Privacy & Security > Screen & System Audio Recording, then restart snapr";

/// What to do when snapr can't watch the keyboard for hotkeys. macOS 27
/// renamed Accessibility; the pane's link is unchanged.
pub const ACCESSIBILITY_OFF: &str = "global hotkeys need snapr turned on under System Settings \
    > Privacy & Security > Device Control and Data Access (Accessibility before macOS 27), \
    then snapr restarted";

/// The System Settings pane that fixes the problem `msg` reports, if it's
/// a missing permission.
pub fn privacy_pane_for(msg: &str) -> Option<&'static str> {
    if msg.ends_with(SCREEN_RECORDING_OFF) {
        Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
    } else if msg.ends_with(ACCESSIBILITY_OFF) {
        Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
    } else {
        None
    }
}

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
    if let Some(ns_window) = ns_window(window) {
        ns_window.setLevel(LEVEL);
        ns_window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
    }
}

/// How far down from the top of `window`'s screen things are clear of the
/// camera notch, in points; 0 on screens without one.
pub fn top_inset(window: &Window) -> f32 {
    let Some(screen) = ns_window(window).and_then(|w| w.screen()) else {
        return 0.0;
    };
    // macOS 12 brought safe areas, and the notch with them.
    if !screen.respondsToSelector(sel!(safeAreaInsets)) {
        return 0.0;
    }
    screen.safeAreaInsets().top as f32
}

/// The AppKit window behind one of winit's.
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return None;
    };
    // SAFETY: winit's view, made on this (the main) thread, alive while
    // `window` is.
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    view.window()
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

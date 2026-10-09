//! macOS menu bar item via `NSStatusItem`.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSSize, NSString};

use super::{Callback, TrayAction, capture_label};
use crate::settings::ToolAction;

struct Ivars {
    callback: Callback,
}

define_class!(
    // Receives menu item actions and forwards them to the callback.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnaprTrayTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(capture:))]
        fn capture(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Capture);
        }

        #[unsafe(method(pickColor:))]
        fn pick_color(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Tool(ToolAction::PickColor));
        }

        #[unsafe(method(scanQr:))]
        fn scan_qr(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Tool(ToolAction::ScanQr));
        }

        #[unsafe(method(pinRegion:))]
        fn pin_region(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Tool(ToolAction::PinRegion));
        }

        #[unsafe(method(recent:))]
        fn recent(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Recent);
        }

        #[unsafe(method(settings:))]
        fn settings(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Settings);
        }

        #[unsafe(method(openFolder:))]
        fn open_folder(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::OpenFolder);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callback)(TrayAction::Quit);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker, callback: Callback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { callback });
        unsafe { msg_send![super(this), init] }
    }
}

pub struct Tray {
    status_item: Retained<NSStatusItem>,
    capture_item: Retained<NSMenuItem>,
    _menu: Retained<NSMenu>,
    _target: Retained<Target>,
}

impl Tray {
    pub fn new(hotkey: Option<&str>, callback: Callback) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("the tray must be created on the main thread")?;
        let target = Target::new(mtm, callback);
        let menu = NSMenu::new(mtm);
        let add_to = |menu: &NSMenu, title: &str, action: Option<Sel>| {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    action,
                    &NSString::from_str(""),
                )
            };
            unsafe { item.setTarget(Some(&target)) };
            menu.addItem(&item);
            item
        };
        let add = |title: &str, action: Sel| add_to(&menu, title, Some(action));
        let capture_item = add(&capture_label(hotkey), sel!(capture:));
        let tools = NSMenu::new(mtm);
        for tool in ToolAction::ALL {
            let action = match tool {
                ToolAction::PickColor => sel!(pickColor:),
                ToolAction::ScanQr => sel!(scanQr:),
                ToolAction::PinRegion => sel!(pinRegion:),
            };
            add_to(&tools, tool.label(), Some(action));
        }
        add_to(&menu, "Tools", None).setSubmenu(Some(&tools));
        add("Recent screenshots\u{2026}", sel!(recent:));
        add("Settings\u{2026}", sel!(settings:));
        add("Open screenshots folder", sel!(openFolder:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Quit snapr", sel!(quit:));

        let status_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            if let Some(image) = icon_image() {
                button.setImage(Some(&image));
            }
            button.setToolTip(Some(&NSString::from_str("snapr")));
        }
        status_item.setMenu(Some(&menu));

        Ok(Self {
            status_item,
            capture_item,
            _menu: menu,
            _target: target,
        })
    }

    pub fn set_hotkey(&self, hotkey: Option<&str>) {
        self.capture_item
            .setTitle(&NSString::from_str(&capture_label(hotkey)));
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.status_item);
    }
}

/// The icon as a template image, so macOS tints it to match the menu bar.
fn icon_image() -> Option<Retained<NSImage>> {
    const SIZE: u32 = 36;
    let img = image::RgbaImage::from_raw(SIZE, SIZE, super::icon_rgba(SIZE))?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&png))?;
    image.setSize(NSSize::new(18.0, 18.0));
    image.setTemplate(true);
    Some(image)
}

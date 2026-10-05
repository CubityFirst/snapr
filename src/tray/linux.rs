//! Linux tray icon via the StatusNotifierItem D-Bus protocol (KDE, most
//! desktops; GNOME needs the AppIndicator extension).

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};

use super::{Callback, TrayAction, capture_label};

struct Sni {
    capture_label: String,
    callback: Callback,
}

impl ksni::Tray for Sni {
    fn id(&self) -> String {
        "snapr".into()
    }

    fn title(&self) -> String {
        "snapr".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        const SIZE: u32 = 32;
        // RGBA -> ARGB, as the protocol requires.
        let data = super::icon_rgba(SIZE)
            .chunks_exact(4)
            .flat_map(|p| [p[3], p[0], p[1], p[2]])
            .collect();
        vec![ksni::Icon {
            width: SIZE as i32,
            height: SIZE as i32,
            data,
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.callback)(TrayAction::Recent);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: String, action: TrayAction| -> MenuItem<Self> {
            StandardItem {
                label,
                activate: Box::new(move |t: &mut Self| (t.callback)(action)),
                ..Default::default()
            }
            .into()
        };
        vec![
            item(self.capture_label.clone(), TrayAction::Capture),
            item("Recent screenshots\u{2026}".into(), TrayAction::Recent),
            item("Settings\u{2026}".into(), TrayAction::Settings),
            item("Open screenshots folder".into(), TrayAction::OpenFolder),
            MenuItem::Separator,
            item("Quit snapr".into(), TrayAction::Quit),
        ]
    }
}

pub struct Tray {
    handle: Handle<Sni>,
}

impl Tray {
    pub fn new(hotkey: Option<&str>, callback: Callback) -> Result<Self, String> {
        let sni = Sni {
            capture_label: capture_label(hotkey),
            callback,
        };
        let handle = sni.spawn().map_err(|e| format!("tray unavailable: {e}"))?;
        Ok(Self { handle })
    }

    pub fn set_hotkey(&self, hotkey: Option<&str>) {
        let label = capture_label(hotkey);
        self.handle.update(|t| t.capture_label = label);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown().wait();
    }
}

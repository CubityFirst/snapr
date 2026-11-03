//! wlroots compositors' own protocols (Wayfire, Sway, river, labwc,
//! Hyprland), for what xcap can't do there: it sees only XWayland's windows.
//!
//! - `zwlr_foreign_toplevel_management_v1`: the focused window's app id and
//!   title, for naming screenshots.
//!
//! Each call makes its own connection, so it works from any thread. Where
//! there's no Wayland compositor or it lacks a protocol, they return `None`
//! and callers fall back to xcap.

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1 as handle, zwlr_foreign_toplevel_manager_v1 as toplevel_manager,
};

/// The focused window's app id and title, as the compositor tells it
/// (either may be missing; both are when nothing is focused). `None` if
/// it doesn't (not a Wayland session, or no foreign-toplevel: GNOME, KDE).
pub fn active_window() -> Option<(Option<String>, Option<String>)> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Toplevels>(&conn).ok()?;
    let qh = queue.handle();
    let manager: toplevel_manager::ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 1..=3, ()).ok()?;
    let mut state = Toplevels::default();
    // Binding the manager announces every window, each followed by its
    // details and `done`.
    queue.roundtrip(&mut state).ok()?;
    manager.stop();
    let found = state
        .windows
        .iter()
        .find(|w| w.done && w.current.activated)
        .map(|w| {
            let named = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
            (named(&w.current.app_id), named(&w.current.title))
        });
    Some(found.unwrap_or((None, None)))
}

#[derive(Default)]
struct Toplevels {
    windows: Vec<Toplevel>,
}

struct Toplevel {
    handle: handle::ZwlrForeignToplevelHandleV1,
    /// Details as of the last `done`, and those since (the protocol sends
    /// changes in batches, each ended by `done`).
    current: Details,
    pending: Details,
    done: bool,
}

#[derive(Default, Clone)]
struct Details {
    app_id: Option<String>,
    title: Option<String>,
    activated: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Toplevels {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<toplevel_manager::ZwlrForeignToplevelManagerV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        _: &toplevel_manager::ZwlrForeignToplevelManagerV1,
        event: toplevel_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let toplevel_manager::Event::Toplevel { toplevel } = event {
            state.windows.push(Toplevel {
                handle: toplevel,
                current: Details::default(),
                pending: Details::default(),
                done: false,
            });
        }
    }

    event_created_child!(Toplevels, toplevel_manager::ZwlrForeignToplevelManagerV1, [
        toplevel_manager::EVT_TOPLEVEL_OPCODE => (handle::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<handle::ZwlrForeignToplevelHandleV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        proxy: &handle::ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = state.windows.iter().position(|w| w.handle == *proxy) else {
            return;
        };
        let window = &mut state.windows[i];
        match event {
            handle::Event::Title { title } => window.pending.title = Some(title),
            handle::Event::AppId { app_id } => window.pending.app_id = Some(app_id),
            handle::Event::State { state: states } => {
                // An array of native-endian u32 enum values.
                let activated = handle::State::Activated as u32;
                window.pending.activated = states.as_chunks::<4>().0.iter().any(|s| u32::from_ne_bytes(*s) == activated);
            }
            handle::Event::Done => {
                window.current = window.pending.clone();
                window.done = true;
            }
            handle::Event::Closed => {
                state.windows.remove(i).handle.destroy();
            }
            _ => {}
        }
    }
}

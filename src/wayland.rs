//! wlroots compositors' own protocols (Wayfire, Sway, river, labwc,
//! Hyprland), for what xcap can't do there. xcap sees only XWayland's
//! windows and monitors, and records through the desktop's ScreenCast
//! portal, which Wayfire doesn't provide and which asks which screen to share.
//!
//! - `zwlr_foreign_toplevel_management_v1`: the focused window's app id and
//!   title, for naming screenshots.
//! - `zwlr_screencopy_v1` (with `xdg_output` for the layout): pictures of
//!   the outputs, for screenshots (xcap asks the Screenshot portal for the
//!   whole desktop once per monitor), and frames of one, for recording.
//!
//! Each call makes its own connection, so it works from any thread. Where
//! there's no Wayland compositor or it lacks a protocol, they return `None`
//! (or an error) and callers fall back to xcap.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1 as handle, zwlr_foreign_toplevel_manager_v1 as toplevel_manager,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as screencopy_manager,
};
use xcap::Frame;

use crate::capture::Rect;

/// The focused window, as the compositor tells it. Every field may be
/// missing; all are when nothing is focused.
#[derive(Debug, Default)]
pub struct Focused {
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// The output it's on, by name (e.g. `DP-1`): the first it entered, of
    /// those it's still on.
    pub output: Option<String>,
}

/// The focused window, as the compositor tells it. `None` if it doesn't
/// (not a Wayland session, or no foreign-toplevel: GNOME, KDE).
pub fn active_window() -> Option<Focused> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Toplevels>(&conn).ok()?;
    let qh = queue.handle();
    // The outputs first, so the manager can say which a window is on as it
    // announces it. Their names come from wl_output (version 4) or, before
    // that, xdg-output.
    let xdg: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1> = globals.bind(&qh, 1..=3, ()).ok();
    let mut state = Toplevels {
        outputs: globals
            .contents()
            .clone_list()
            .into_iter()
            .filter(|g| g.interface == wl_output::WlOutput::interface().name)
            .map(|g| {
                let output: wl_output::WlOutput = globals.registry().bind(g.name, g.version.min(4), &qh, ());
                NamedOutput {
                    xdg: xdg.as_ref().map(|m| m.get_xdg_output(&output, &qh, ())),
                    output,
                    name: None,
                }
            })
            .collect(),
        ..Toplevels::default()
    };
    let manager: toplevel_manager::ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 1..=3, ()).ok()?;
    // Binding the manager announces every window, each followed by its
    // details and `done`.
    queue.roundtrip(&mut state).ok()?;
    queue.roundtrip(&mut state).ok()?;
    manager.stop();
    let focused = state
        .windows
        .iter()
        .find(|w| w.done && w.current.activated)
        .map(|w| {
            let named = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
            Focused {
                app_id: named(&w.current.app_id),
                title: named(&w.current.title),
                output: w.current.outputs.iter().find_map(|on| {
                    state.outputs.iter().find(|o| o.output == *on).and_then(|o| named(&o.name))
                }),
            }
        });
    Some(focused.unwrap_or_default())
}

#[derive(Default)]
struct Toplevels {
    windows: Vec<Toplevel>,
    /// Every output, to name the ones a window is on.
    outputs: Vec<NamedOutput>,
}

struct NamedOutput {
    output: wl_output::WlOutput,
    xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
    name: Option<String>,
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
    /// The outputs it's on, in the order it entered them.
    outputs: Vec<wl_output::WlOutput>,
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
                window.pending.activated =
                    states.as_chunks::<4>().0.iter().any(|s| u32::from_ne_bytes(*s) == activated);
            }
            // Applied straight away too, in case a compositor doesn't
            // follow them with `done`.
            handle::Event::OutputEnter { output } => {
                for details in [&mut window.pending, &mut window.current] {
                    if !details.outputs.contains(&output) {
                        details.outputs.push(output.clone());
                    }
                }
            }
            handle::Event::OutputLeave { output } => {
                for details in [&mut window.pending, &mut window.current] {
                    details.outputs.retain(|o| *o != output);
                }
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

impl Dispatch<wl_output::WlOutput, ()> for Toplevels {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(o) = state.outputs.iter_mut().find(|o| o.output == *proxy)
        {
            o.name = Some(name);
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        proxy: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // As in `ScreenState`: wl_output's name wins, where it has one.
        if let zxdg_output_v1::Event::Name { name } = event
            && let Some(o) = state.outputs.iter_mut().find(|o| o.xdg.as_ref() == Some(proxy))
        {
            o.name.get_or_insert(name);
        }
    }
}

delegate_noop!(Toplevels: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

/// A monitor as winit places it, so it lines up with a selection on the
/// overlays: at the compositor's logical position times the output's
/// (integer) scale, and its mode's size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    pub rect: Rect,
}

/// The monitors, if the compositor can screencopy them and says where they
/// are (xdg-output).
pub fn screencopy_outputs() -> Option<Vec<Output>> {
    let screens = Screens::connect().ok()?;
    let outputs: Vec<Output> = screens.state.outputs.iter().filter_map(OutputState::placed).collect();
    (!outputs.is_empty()).then_some(outputs)
}

/// A picture of each output now, placed as `screencopy_outputs` places
/// them, with the mouse pointer in if `cursor`. `None` if the compositor
/// can't screencopy, or can't one of them, so xcap takes them all instead;
/// rotated or flipped outputs are left to xcap too, as screencopy gives
/// their pictures untransformed.
pub fn screenshot(cursor: bool) -> Option<Vec<(Rect, Frame)>> {
    let mut screens = Screens::connect().ok()?;
    screens.cursor = cursor;
    let outputs: Option<Vec<_>> = screens
        .state
        .outputs
        .iter()
        .map(|o| {
            if o.transform != wl_output::Transform::Normal {
                return None;
            }
            Some((o.output.clone(), o.placed()?))
        })
        .collect();
    let outputs = outputs.filter(|o| !o.is_empty())?;
    outputs
        .into_iter()
        .map(|(output, placed)| match screens.capture(&output, false, Some(FIRST_FRAME_TIMEOUT), &|| false) {
            Ok(frame) => Some((placed.rect, frame?)),
            Err(e) => {
                eprintln!("screencopy of {}: {e}; asking xcap", placed.name);
                None
            }
        })
        .collect()
}

/// A picture of one output now, placed as `screencopy_outputs` places it,
/// with the mouse pointer in if `cursor`: the one named `name`, else (no
/// name, or no output by it) the one at the origin, else the first. `None`
/// as for `screenshot`, so xcap takes it instead.
pub fn screenshot_output(name: Option<&str>, cursor: bool) -> Option<(Output, Frame)> {
    let mut screens = Screens::connect().ok()?;
    screens.cursor = cursor;
    let outputs = &screens.state.outputs;
    let named = name.and_then(|n| outputs.iter().find(|o| o.name.as_deref() == Some(n)));
    let at_origin = || {
        outputs
            .iter()
            .find(|o| o.placed().is_some_and(|p| p.rect.contains((0.0, 0.0))))
    };
    let state = named.or_else(at_origin).or(outputs.first())?;
    if state.transform != wl_output::Transform::Normal {
        return None;
    }
    let (output, placed) = (state.output.clone(), state.placed()?);
    match screens.capture(&output, false, Some(FIRST_FRAME_TIMEOUT), &|| false) {
        Ok(frame) => Some((placed, frame?)),
        Err(e) => {
            eprintln!("screencopy of {}: {e}; asking xcap", placed.name);
            None
        }
    }
}

/// Frames of one output, captured on a thread of its own.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Starts capturing the output named `name`, at most `fps` times a
    /// second and only when it changes, with its picture now, and the mouse
    /// pointer in them if `cursor`. Frames are the whole output, in RGBA.
    pub fn start(name: &str, fps: u32, cursor: bool) -> Result<(Self, Receiver<Frame>, Frame), String> {
        let mut screens = Screens::connect()?;
        screens.cursor = cursor;
        let output = screens
            .state
            .outputs
            .iter()
            .find(|o| o.name.as_deref() == Some(name))
            .map(|o| o.output.clone())
            .ok_or_else(|| format!("no output named {name}"))?;
        let first = screens
            .capture(&output, false, Some(FIRST_FRAME_TIMEOUT), &|| false)?
            .ok_or("the compositor didn't send a first frame")?;
        // Room for one frame waiting: the recording takes the newest, and
        // ones it doesn't want aren't kept.
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        let thread = thread::Builder::new()
            .name("screencopy".into())
            .spawn(move || capture_loop(screens, output, interval, &thread_stop, tx))
            .map_err(|e| format!("couldn't start capturing: {e}"))?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            rx,
            first,
        ))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How long a first frame may take: the compositor sends one once it next
/// draws the output, which it's asked to do straight away.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a capture waiting for the screen to change checks whether
/// it's still wanted.
const STOP_CHECK: Duration = Duration::from_millis(100);

fn capture_loop(mut screens: Screens, output: wl_output::WlOutput, interval: Duration, stop: &AtomicBool, tx: SyncSender<Frame>) {
    let stopped = || stop.load(Ordering::Relaxed);
    let mut failures = 0;
    while !stopped() {
        let began = Instant::now();
        // Only once the screen changes: until then the recording repeats
        // the last frame.
        match screens.capture(&output, true, None, &stopped) {
            Ok(Some(frame)) => {
                failures = 0;
                if !deliver(&tx, frame, &stopped) {
                    return;
                }
            }
            Ok(None) => {}
            // Say, the output is turned off for a moment. The recording
            // keeps its last frame meanwhile.
            Err(e) => {
                failures += 1;
                if failures == 1 {
                    eprintln!("screencopy: {e}");
                }
                if failures >= 50 {
                    eprintln!("screencopy: giving up on this output");
                    return;
                }
                thread::sleep(STOP_CHECK);
            }
        }
        thread::sleep(interval.saturating_sub(began.elapsed()));
    }
}

/// Hands `frame` to the recording, waiting while it hasn't taken the last
/// one yet: the screen may not change again, so a dropped frame might never
/// be replaced. `false` once the recording is gone or `stopped`.
fn deliver(tx: &SyncSender<Frame>, mut frame: Frame, stopped: &dyn Fn() -> bool) -> bool {
    loop {
        match tx.try_send(frame) {
            Ok(()) => return true,
            Err(TrySendError::Full(f)) if !stopped() => {
                frame = f;
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

/// A connection with what screencopy needs bound.
struct Screens {
    conn: Connection,
    queue: EventQueue<ScreenState>,
    state: ScreenState,
    manager: screencopy_manager::ZwlrScreencopyManagerV1,
    shm: wl_shm::WlShm,
    /// The buffer the last frame was copied into, kept for the next.
    buffer: Option<ShmBuffer>,
    /// Frames include the mouse pointer.
    cursor: bool,
}

#[derive(Default)]
struct ScreenState {
    outputs: Vec<OutputState>,
    frame: FrameState,
}

struct OutputState {
    output: wl_output::WlOutput,
    xdg: zxdg_output_v1::ZxdgOutputV1,
    name: Option<String>,
    logical: Option<(i32, i32)>,
    scale: i32,
    mode: Option<(i32, i32)>,
    transform: wl_output::Transform,
}

impl OutputState {
    fn placed(&self) -> Option<Output> {
        let (x, y) = self.logical?;
        let (w, h) = self.mode?;
        Some(Output {
            name: self.name.clone()?,
            rect: Rect {
                x: x * self.scale,
                y: y * self.scale,
                w: u32::try_from(w).ok()?,
                h: u32::try_from(h).ok()?,
            },
        })
    }
}

/// What the compositor has said about the frame being captured.
#[derive(Default)]
struct FrameState {
    shm: Option<BufferInfo>,
    /// It's sent everything about the buffer it wants (`buffer_done`, or
    /// `buffer` before version 3).
    described: bool,
    y_invert: bool,
    outcome: Option<Result<(), String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferInfo {
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

/// Shared memory a frame is copied into.
struct ShmBuffer {
    info: BufferInfo,
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // The memfd closes with `file`.
    }
}

impl Screens {
    fn connect() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland compositor: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<ScreenState>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let missing = |what: &str| format!("the compositor doesn't support {what}");
        // Version 2 for `copy_with_damage`.
        let manager = globals.bind(&qh, 2..=3, ()).map_err(|_| missing("wlr-screencopy"))?;
        let shm = globals.bind(&qh, 1..=1, ()).map_err(|_| missing("wl_shm"))?;
        let xdg: zxdg_output_manager_v1::ZxdgOutputManagerV1 =
            globals.bind(&qh, 1..=3, ()).map_err(|_| missing("xdg-output"))?;
        let mut state = ScreenState {
            outputs: bind_outputs(&globals, &qh, &xdg),
            ..ScreenState::default()
        };
        // Each output's details, from wl_output and xdg-output.
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            queue,
            state,
            manager,
            shm,
            buffer: None,
            cursor: false,
        })
    }

    /// Captures `output`, or with `damage`, waits until it changes and
    /// captures it then. `None` if `give_up` says so while waiting.
    fn capture(
        &mut self,
        output: &wl_output::WlOutput,
        damage: bool,
        timeout: Option<Duration>,
        give_up: &dyn Fn() -> bool,
    ) -> Result<Option<Frame>, String> {
        let qh = self.queue.handle();
        self.state.frame = FrameState::default();
        let frame = self.manager.capture_output(self.cursor.into(), output, &qh, ());
        let result = self.copy(&frame, damage, timeout, give_up);
        frame.destroy();
        result
    }

    fn copy(
        &mut self,
        frame: &frame::ZwlrScreencopyFrameV1,
        damage: bool,
        timeout: Option<Duration>,
        give_up: &dyn Fn() -> bool,
    ) -> Result<Option<Frame>, String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        if !self.wait(|f| f.described || f.outcome.is_some(), deadline, give_up)? {
            return Ok(None);
        }
        if let Some(Err(e)) = &self.state.frame.outcome {
            return Err(e.clone());
        }
        let info = self.state.frame.shm.ok_or("the compositor offered no shared-memory buffer")?;
        let layout = layout(info.format).ok_or_else(|| format!("unsupported pixel format {:?}", info.format))?;
        if self.buffer.as_ref().is_none_or(|b| b.info != info) {
            self.buffer = None;
            self.buffer = Some(self.shm_buffer(info)?);
        }
        let buffer = &self.buffer.as_ref().expect("just made").buffer;
        if damage {
            frame.copy_with_damage(buffer);
        } else {
            frame.copy(buffer);
        }
        if !self.wait(|f| f.outcome.is_some(), deadline, give_up)? {
            return Ok(None);
        }
        if let Some(Err(e)) = &self.state.frame.outcome {
            return Err(e.clone());
        }
        let file = &self.buffer.as_ref().expect("just made").file;
        let mut raw = vec![0; info.stride as usize * info.height as usize];
        file.read_exact_at(&mut raw, 0).map_err(|e| format!("couldn't read the frame: {e}"))?;
        let rgba = to_rgba(&raw, info.width, info.height, info.stride, layout, self.state.frame.y_invert)
            .ok_or("the frame was smaller than described")?;
        Ok(Some(Frame::new(info.width, info.height, rgba)))
    }

    fn shm_buffer(&self, info: BufferInfo) -> Result<ShmBuffer, String> {
        let size = info.stride as u64 * info.height as u64;
        let fd = rustix::fs::memfd_create("snapr-screencopy", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| format!("couldn't make shared memory: {e}"))?;
        let file = File::from(fd);
        file.set_len(size).map_err(|e| format!("couldn't make shared memory: {e}"))?;
        let size = i32::try_from(size).map_err(|_| "the frame is too big")?;
        let qh = self.queue.handle();
        let pool = self.shm.create_pool(std::os::fd::AsFd::as_fd(&file), size, &qh, ());
        let buffer = pool.create_buffer(
            0,
            info.width as i32,
            info.height as i32,
            info.stride as i32,
            info.format,
            &qh,
            (),
        );
        Ok(ShmBuffer { info, file, pool, buffer })
    }

    /// Handles events until `done` says so (`true`), `deadline` passes (an
    /// error) or `give_up` says to (`false`).
    fn wait(
        &mut self,
        done: impl Fn(&FrameState) -> bool,
        deadline: Option<Instant>,
        give_up: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        let fail = |e: &dyn std::fmt::Display| format!("lost the compositor: {e}");
        loop {
            self.queue.dispatch_pending(&mut self.state).map_err(|e| fail(&e))?;
            if done(&self.state.frame) {
                return Ok(true);
            }
            if give_up() {
                return Ok(false);
            }
            let mut slice = STOP_CHECK;
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("the compositor didn't send the frame in time".into());
                }
                slice = slice.min(left);
            }
            self.conn.flush().map_err(|e| fail(&e))?;
            // Another thread may have read our events already; then
            // they're dispatched next time round.
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let ready = {
                let fd = guard.connection_fd();
                let mut fds = [PollFd::new(&fd, PollFlags::IN | PollFlags::ERR)];
                let timeout = Timespec {
                    tv_sec: slice.as_secs() as _,
                    tv_nsec: slice.subsec_nanos() as _,
                };
                match rustix::event::poll(&mut fds, Some(&timeout)) {
                    Ok(n) => n > 0,
                    Err(rustix::io::Errno::INTR) => false,
                    Err(e) => return Err(fail(&e)),
                }
            };
            if ready {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(fail(&e)),
                }
            }
        }
    }
}

/// Binds every output, and asks for each one's place in the layout.
fn bind_outputs(
    globals: &GlobalList,
    qh: &QueueHandle<ScreenState>,
    xdg: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
) -> Vec<OutputState> {
    globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface == wl_output::WlOutput::interface().name)
        .map(|g| {
            let output: wl_output::WlOutput = globals.registry().bind(g.name, g.version.min(4), qh, ());
            OutputState {
                xdg: xdg.get_xdg_output(&output, qh, ()),
                output,
                name: None,
                logical: None,
                scale: 1,
                mode: None,
                transform: wl_output::Transform::Normal,
            }
        })
        .collect()
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ScreenState {
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

impl Dispatch<wl_output::WlOutput, ()> for ScreenState {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.iter_mut().find(|o| o.output == *proxy) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => o.name = Some(name),
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            wl_output::Event::Geometry { transform: WEnum::Value(transform), .. } => o.transform = transform,
            wl_output::Event::Mode { flags: WEnum::Value(flags), width, height, .. }
                if flags.contains(wl_output::Mode::Current) =>
            {
                o.mode = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for ScreenState {
    fn event(
        state: &mut Self,
        proxy: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.iter_mut().find(|o| o.xdg == *proxy) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => o.logical = Some((x, y)),
            // wl_output's own name (version 4) is the same; older outputs
            // only have this one.
            zxdg_output_v1::Event::Name { name } => {
                o.name.get_or_insert(name);
            }
            _ => {}
        }
    }
}

impl Dispatch<frame::ZwlrScreencopyFrameV1, ()> for ScreenState {
    fn event(
        state: &mut Self,
        proxy: &frame::ZwlrScreencopyFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let f = &mut state.frame;
        match event {
            frame::Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                f.shm = Some(BufferInfo { format, width, height, stride });
                // Before version 3 that's all it sends.
                if proxy.version() < 3 {
                    f.described = true;
                }
            }
            frame::Event::BufferDone => f.described = true,
            frame::Event::Flags { flags: WEnum::Value(flags) } => {
                f.y_invert = flags.contains(frame::Flags::YInvert);
            }
            frame::Event::Ready { .. } => f.outcome = Some(Ok(())),
            frame::Event::Failed => f.outcome = Some(Err("the compositor couldn't copy the screen".into())),
            _ => {}
        }
    }
}

delegate_noop!(ScreenState: ignore wl_shm::WlShm);
delegate_noop!(ScreenState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(ScreenState: ignore wl_buffer::WlBuffer);
delegate_noop!(ScreenState: ignore screencopy_manager::ZwlrScreencopyManagerV1);
delegate_noop!(ScreenState: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

/// How a pixel format's bytes are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// 4 bytes: blue, green, red, unused (`[AX]RGB8888`, little-endian).
    Bgrx,
    /// 4 bytes: red, green, blue, unused (`[AX]BGR8888`).
    Rgbx,
    /// 3 bytes: blue, green, red (`RGB888`).
    Bgr,
    /// 3 bytes: red, green, blue (`BGR888`).
    Rgb,
    /// A little-endian u32 with 10 bits each of red, green and blue from
    /// the top (`[AX]RGB2101010`).
    Rgb10,
    /// The same with blue at the top (`[AX]BGR2101010`).
    Bgr10,
}

fn layout(format: wl_shm::Format) -> Option<Layout> {
    use wl_shm::Format as F;
    Some(match format {
        F::Argb8888 | F::Xrgb8888 => Layout::Bgrx,
        F::Abgr8888 | F::Xbgr8888 => Layout::Rgbx,
        F::Rgb888 => Layout::Bgr,
        F::Bgr888 => Layout::Rgb,
        F::Argb2101010 | F::Xrgb2101010 => Layout::Rgb10,
        F::Abgr2101010 | F::Xbgr2101010 => Layout::Bgr10,
        _ => return None,
    })
}

/// `height` rows of `stride` bytes of `layout` pixels (bottom row first if
/// `y_invert`) as opaque RGBA rows without padding. `None` if `src` is too
/// short for that.
fn to_rgba(src: &[u8], width: u32, height: u32, stride: u32, layout: Layout, y_invert: bool) -> Option<Vec<u8>> {
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    let bytes = match layout {
        Layout::Bgr | Layout::Rgb => 3,
        _ => 4,
    };
    if stride < w * bytes || src.len() < stride * h.saturating_sub(1) + w * bytes {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = if y_invert { h - 1 - y } else { y };
        let row = &src[row * stride..row * stride + w * bytes];
        for px in row.chunks_exact(bytes) {
            // A 10-bit channel's top 8 bits.
            let ten = |shift: u32| {
                let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                (((v >> shift) & 0x3ff) >> 2) as u8
            };
            let [r, g, b] = match layout {
                Layout::Bgrx | Layout::Bgr => [px[2], px[1], px[0]],
                Layout::Rgbx | Layout::Rgb => [px[0], px[1], px[2]],
                Layout::Rgb10 => [ten(20), ten(10), ten(0)],
                Layout::Bgr10 => [ten(0), ten(10), ten(20)],
            };
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bgrx_dropping_padding() {
        // 2x2, rows padded to 12 bytes; alpha bytes are junk.
        let src = [
            1, 2, 3, 0, 4, 5, 6, 9, 0xee, 0xee, 0xee, 0xee, //
            7, 8, 9, 0, 10, 11, 12, 0, 0xee, 0xee, 0xee, 0xee,
        ];
        let rgba = to_rgba(&src, 2, 2, 12, Layout::Bgrx, false).unwrap();
        assert_eq!(rgba, [3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]);
    }

    #[test]
    fn flips_inverted_frames() {
        let src = [1, 2, 3, 0, 4, 5, 6, 0];
        let rgba = to_rgba(&src, 1, 2, 4, Layout::Rgbx, true).unwrap();
        assert_eq!(rgba, [4, 5, 6, 255, 1, 2, 3, 255]);
    }

    #[test]
    fn converts_three_byte_and_ten_bit_pixels() {
        assert_eq!(to_rgba(&[1, 2, 3], 1, 1, 3, Layout::Bgr, false).unwrap(), [3, 2, 1, 255]);
        assert_eq!(to_rgba(&[1, 2, 3], 1, 1, 3, Layout::Rgb, false).unwrap(), [1, 2, 3, 255]);
        // Red full, green half, blue none.
        let v: u32 = (0x3ff << 20) | (0x200 << 10);
        let rgba = to_rgba(&v.to_le_bytes(), 1, 1, 4, Layout::Rgb10, false).unwrap();
        assert_eq!(rgba, [255, 128, 0, 255]);
        let rgba = to_rgba(&v.to_le_bytes(), 1, 1, 4, Layout::Bgr10, false).unwrap();
        assert_eq!(rgba, [0, 128, 255, 255]);
    }

    /// Asks the running compositor for the focused window and the outputs,
    /// and records a second of each output, leaving the first and last
    /// frames in the temp folder to look at:
    /// `cargo test wayland::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live() {
        let focused = active_window();
        println!("focused window: {focused:?}");
        let outputs = screencopy_outputs().expect("screencopy and xdg-output");
        println!("outputs: {outputs:?}");
        let began = Instant::now();
        let shots = screenshot(true).expect("a screenshot");
        println!("screenshot of {} outputs in {:?}", shots.len(), began.elapsed());
        let on = focused.and_then(|f| f.output);
        let began = Instant::now();
        let (output, frame) = screenshot_output(on.as_deref(), true).expect("the focused window's output");
        println!("{} (focused window's: {on:?}) in {:?}", output.name, began.elapsed());
        assert_eq!((frame.width, frame.height), (output.rect.w, output.rect.h));
        if let Some(on) = on {
            assert_eq!(output.name, on);
        }
        for output in outputs {
            let began = Instant::now();
            let (recorder, frames, first) = Recorder::start(&output.name, 30, true).unwrap();
            println!("{}: first frame {}x{} in {:?}", output.name, first.width, first.height, began.elapsed());
            assert_eq!((first.width, first.height), (output.rect.w, output.rect.h));
            let mut last = None;
            let end = Instant::now() + Duration::from_secs(1);
            let mut count = 0;
            while Instant::now() < end {
                if let Ok(f) = frames.recv_timeout(Duration::from_millis(50)) {
                    count += 1;
                    last = Some(f);
                }
            }
            drop(recorder);
            println!("{}: {count} more frames in a second (only as the screen changes)", output.name);
            let dir = std::env::temp_dir();
            for (which, f) in [("first", Some(first)), ("last", last)] {
                if let Some(f) = f {
                    let path = dir.join(format!("snapr-wlr-{}-{which}.png", output.name));
                    image::RgbaImage::from_raw(f.width, f.height, f.raw).unwrap().save(&path).unwrap();
                    println!("  {}", path.display());
                }
            }
        }
    }

    #[test]
    fn a_frame_waits_for_room_rather_than_being_dropped() {
        let (tx, rx) = mpsc::sync_channel(1);
        // The recording hasn't taken the last frame yet.
        tx.send(Frame::new(1, 1, vec![1; 4])).unwrap();
        let recording = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let old = rx.recv().unwrap();
            let new = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            (old.raw[0], new.raw[0])
        });
        assert!(deliver(&tx, Frame::new(1, 1, vec![2; 4]), &|| false));
        assert_eq!(recording.join().unwrap(), (1, 2));
    }

    #[test]
    fn a_waiting_frame_is_given_up_once_stopped() {
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.send(Frame::new(1, 1, vec![1; 4])).unwrap();
        assert!(!deliver(&tx, Frame::new(1, 1, vec![2; 4]), &|| true));
    }

    #[test]
    fn short_frames_are_refused() {
        assert!(to_rgba(&[0; 7], 2, 1, 8, Layout::Bgrx, false).is_none());
        assert!(to_rgba(&[0; 8], 2, 1, 4, Layout::Bgrx, false).is_none());
    }
}

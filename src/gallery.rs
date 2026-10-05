//! The Recent page: a large preview of the selected screenshot, recording or
//! upload and a grid of thumbnails. Previews are made on a background thread
//! (see `thumbnail`); files without a picture get a file icon. Videos play
//! in the large preview (see `player`).
//!
//! Images can be combined: Ctrl-click several and right-click one, or drag one
//! onto another and drop it on *Horizontal* or *Vertical*.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use egui::{Color32, CornerRadius, RichText, Sense, Stroke, StrokeKind, Vec2, vec2};

use crate::combine::Direction;
use crate::history::{self, Entry, Remote};
use crate::player::Player;
use crate::thumbnail::{self, Kind};

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
const CARD: Vec2 = vec2(176.0, 112.0);
/// Largest preview size to decode, in pixels.
const LARGE: (u32, u32) = (1920, 1200);
const SMALL: (u32, u32) = (352, 224);
/// How many screenshots the page lists.
const MAX_ITEMS: usize = 200;

pub enum Action {
    Capture,
    Open(PathBuf),
    ShowInFolder(PathBuf),
    Copy(PathBuf),
    CopyLink(String),
    Delete(PathBuf),
    /// Delete the uploaded copy from the storage bucket.
    DeleteRemote(PathBuf, Remote),
    /// Stitch these images together, in order, into a new screenshot.
    Combine(Vec<PathBuf>, Direction),
    /// Show the image on top of everything, in its own window.
    Pin(PathBuf),
}

enum Thumb {
    Loading,
    Ready {
        texture: egui::TextureHandle,
        size: [u32; 2],
    },
    Failed,
}

type Loaded = (PathBuf, bool, Option<(egui::ColorImage, [u32; 2])>);

pub struct Gallery {
    items: Vec<Entry>,
    selected: Option<PathBuf>,
    /// Ctrl-clicked screenshots, in the order they were picked.
    picked: Vec<PathBuf>,
    /// The card being dragged onto another to combine them.
    dragging: Option<PathBuf>,
    /// Keyed by path and whether it's the large preview.
    thumbs: HashMap<(PathBuf, bool), Thumb>,
    requests: Sender<(PathBuf, bool)>,
    results: Receiver<Loaded>,
    /// "Clear recent" was picked from a context menu this frame.
    clear_requested: bool,
    /// Result of the last remote delete; `true` = error.
    pub notice: Option<(String, bool)>,
    ffmpeg: String,
    /// The video playing in the large preview.
    player: Option<Player>,
    /// Where the seek bar is being dragged to, 0-1.
    scrub: Option<f32>,
}

impl Gallery {
    /// `wake` is called from the loader thread when a thumbnail is ready.
    /// `ffmpeg` decodes videos where the OS can't (Linux).
    pub fn new(folder: &Path, ffmpeg: String, wake: Box<dyn Fn() + Send>) -> Self {
        let mut items = history::load();
        // Only on first run: after "Clear recent" the list should stay empty.
        if items.is_empty() && history::is_new() {
            items = history::scan(folder, MAX_ITEMS)
                .into_iter()
                .map(Entry::new)
                .collect();
        }
        items.truncate(MAX_ITEMS);
        let (requests, rx) = mpsc::channel::<(PathBuf, bool)>();
        let (tx, results) = mpsc::channel();
        let player_ffmpeg = ffmpeg.clone();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                for (path, large) in rx {
                    let (w, h) = if large { LARGE } else { SMALL };
                    let image = thumbnail::picture(&path, &ffmpeg)
                        .map(|img| thumbnail::to_egui(img, (w, h)));
                    if tx.send((path, large, image)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .expect("spawn thumbnail thread");
        Self {
            items,
            selected: None,
            picked: Vec::new(),
            dragging: None,
            thumbs: HashMap::new(),
            requests,
            results,
            clear_requested: false,
            notice: None,
            ffmpeg: player_ffmpeg,
            player: None,
            scrub: None,
        }
    }

    /// A new screenshot was saved.
    pub fn add(&mut self, path: PathBuf) {
        self.items.retain(|e| e.path != path);
        self.items.insert(0, Entry::new(path));
        self.items.truncate(MAX_ITEMS);
        self.selected = None; // show the newest
        self.picked.clear();
    }

    /// A screenshot was uploaded, or (with `None`s) deleted remotely.
    pub fn set_link(&mut self, path: &Path, link: Option<String>, remote: Option<Remote>) {
        if let Some(e) = self.items.iter_mut().find(|e| e.path == path) {
            e.link = link;
            e.remote = remote;
        }
    }

    pub fn remove(&mut self, path: &Path) {
        self.items.retain(|e| e.path != path);
        self.thumbs.retain(|(p, _), _| p != path);
        self.picked.retain(|p| p != path);
        if self.selected.as_deref() == Some(path) {
            self.selected = None;
        }
    }

    /// Stops the video playing in the preview, if any.
    pub fn stop_playback(&mut self) {
        self.player = None;
        self.scrub = None;
    }

    /// Empties the list (and the history file) without touching the files.
    fn clear(&mut self) {
        history::clear();
        self.items.clear();
        self.thumbs.clear();
        self.selected = None;
        self.picked.clear();
        self.dragging = None;
    }

    /// The "Clear recent" item shared by the context menus.
    fn clear_menu_item(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Clear recent")
            .on_hover_text("Empty this list. The screenshot files are kept.")
            .clicked()
        {
            self.clear_requested = true;
            ui.close();
        }
    }

    fn thumb(&mut self, path: &Path, large: bool) -> &Thumb {
        let key = (path.to_owned(), large);
        if !self.thumbs.contains_key(&key) {
            let _ = self.requests.send(key.clone());
            self.thumbs.insert(key.clone(), Thumb::Loading);
        }
        &self.thumbs[&key]
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok((path, large, image)) = self.results.try_recv() {
            let thumb = match image {
                Some((img, size)) => {
                    let name = format!("{}{}", path.display(), if large { "#large" } else { "" });
                    Thumb::Ready {
                        texture: ctx.load_texture(name, img, egui::TextureOptions::LINEAR),
                        size,
                    }
                }
                None => Thumb::Failed,
            };
            self.thumbs.insert((path, large), thumb);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, hotkey: &str, actions: &mut Vec<Action>) {
        self.receive(ui.ctx());
        ui.horizontal(|ui| {
            ui.heading("Recent");
            if !self.items.is_empty() {
                ui.label(RichText::new(format!("{} screenshots", self.items.len())).weak());
            }
            if let Some((msg, is_err)) = &self.notice {
                let color = if *is_err {
                    crate::settings_ui::ERROR
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(color, msg);
            }
        });
        if !self.picked.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.picked.clear();
        }
        ui.add_space(8.0);

        let Some(selected) = self
            .selected
            .clone()
            .or_else(|| self.items.first().map(|e| e.path.clone()))
        else {
            self.stop_playback();
            empty_state(ui, hotkey, actions);
            return;
        };
        let is_video = thumbnail::kind(&selected) == Kind::Video;
        if self.player.as_ref().is_some_and(|p| p.path != selected) {
            self.stop_playback();
        }

        // Large preview of the selected screenshot.
        let preview_h = (ui.available_height() * 0.55).clamp(160.0, 560.0);
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), preview_h), Sense::click());
        let bg = ui.visuals().extreme_bg_color;
        ui.painter().rect_filled(rect, 8.0, bg);
        let mut full_size = None;
        // The picture to show and its size at full preview quality. While the
        // large preview loads, the grid's thumbnail stands in for it, so the
        // preview doesn't blank out (or jump) on every click.
        let picture = match self.thumb(&selected, true) {
            Thumb::Ready { texture, size } => {
                Ok(Some((texture.clone(), *size, texture.size_vec2())))
            }
            Thumb::Loading => Ok(None),
            Thumb::Failed => Err(()),
        };
        let picture = match picture {
            Ok(None) => match self.thumb(&selected, false) {
                Thumb::Ready { texture, size } => {
                    Ok(Some((texture.clone(), *size, fit(*size, LARGE))))
                }
                _ => Ok(None),
            },
            other => other,
        };
        // A playing video's current frame replaces the poster, at the
        // poster's size so the picture doesn't jump when it starts.
        let frame = self
            .player
            .as_mut()
            .and_then(|p| p.update(ui.ctx()))
            .cloned();
        match picture {
            Ok(Some((texture, size, tex))) => {
                full_size = Some(size);
                let scale = ((rect.width() - 16.0) / tex.x)
                    .min((rect.height() - 16.0) / tex.y)
                    .min(1.0);
                let img_rect = egui::Rect::from_center_size(rect.center(), tex * scale);
                let shown = frame.as_ref().unwrap_or(&texture);
                egui::Image::new((shown.id(), tex))
                    .corner_radius(4.0)
                    .paint_at(ui, img_rect);
                if is_video {
                    match &self.player {
                        None => thumbnail::draw_play_badge(ui.painter(), img_rect.center(), 28.0),
                        Some(p) if p.loading() => egui::Spinner::new().paint_at(
                            ui,
                            egui::Rect::from_center_size(img_rect.center(), vec2(28.0, 28.0)),
                        ),
                        Some(_) => {}
                    }
                }
            }
            // No poster, but the video plays anyway.
            _ if let Some(frame) = &frame => {
                let tex = frame.size_vec2();
                let scale = ((rect.width() - 16.0) / tex.x)
                    .min((rect.height() - 16.0) / tex.y)
                    .min(1.0);
                let img_rect = egui::Rect::from_center_size(rect.center(), tex * scale);
                egui::Image::new((frame.id(), tex))
                    .corner_radius(4.0)
                    .paint_at(ui, img_rect);
            }
            // Painted rather than added, so it doesn't move what follows.
            Ok(None) => egui::Spinner::new().paint_at(
                ui,
                egui::Rect::from_center_size(rect.center(), vec2(24.0, 24.0)),
            ),
            Err(()) if thumbnail::kind(&selected) == Kind::Image => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Couldn't load image",
                    egui::FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
            }
            Err(()) => thumbnail::draw_file_icon(ui.painter(), rect, &selected),
        }
        let hover = if is_video {
            "Click to play or pause, double-click to open"
        } else {
            "Double-click to open"
        };
        let resp = resp.on_hover_text(hover);
        if is_video && resp.clicked() {
            match &mut self.player {
                Some(p) => p.toggle(),
                None => self.player = Some(Player::new(selected.clone(), &self.ffmpeg)),
            }
        }
        if is_video && let Some(p) = &mut self.player {
            if ui.rect_contains_pointer(rect) || p.is_paused() || self.scrub.is_some() {
                video_controls(ui, rect, p, &mut self.scrub);
            }
            if p.failed() {
                ui.painter().text(
                    rect.center_top() + vec2(0.0, 14.0),
                    egui::Align2::CENTER_TOP,
                    "Couldn't play this video",
                    egui::FontId::proportional(13.0),
                    crate::settings_ui::ERROR,
                );
            }
        }
        if resp.double_clicked() {
            actions.push(Action::Open(selected.clone()));
        }

        // Details and actions.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let name = selected
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(RichText::new(name).strong());
                ui.label(RichText::new(details(&selected, full_size)).weak().small());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Delete")
                    .on_hover_text("Move to the Recycle Bin")
                    .clicked()
                {
                    actions.push(Action::Delete(selected.clone()));
                }
                if ui.button("Show in folder").clicked() {
                    actions.push(Action::ShowInFolder(selected.clone()));
                }
                let link = self
                    .items
                    .iter()
                    .find(|e| e.path == selected)
                    .and_then(|e| e.link.clone());
                if let Some(link) = link
                    && ui.button("Copy link").on_hover_text(&link).clicked()
                {
                    actions.push(Action::CopyLink(link));
                }
                if thumbnail::kind(&selected) == Kind::Image
                    && ui
                        .button("Pin")
                        .on_hover_text("Pin the image to the screen, on top of other windows")
                        .clicked()
                {
                    actions.push(Action::Pin(selected.clone()));
                }
                if thumbnail::kind(&selected) == Kind::Image
                    && ui
                        .button("Copy")
                        .on_hover_text("Copy the image to the clipboard")
                        .clicked()
                {
                    actions.push(Action::Copy(selected.clone()));
                }
                if ui.button("Open").clicked() {
                    actions.push(Action::Open(selected.clone()));
                }
            });
        });
        ui.add_space(6.0);
        ui.separator();

        // Thumbnail grid. The background is registered first so the cards
        // on top of it keep their own context menu.
        let background = ui.interact(
            ui.available_rect_before_wrap(),
            ui.id().with("grid-background"),
            Sense::click(),
        );
        background.context_menu(|ui| self.clear_menu_item(ui));
        let mut drop_target = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let gap = 10.0;
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                // As many columns as fit, centred: the space left over that
                // isn't enough for another card goes on both sides.
                let width = ui.available_width();
                let cols = (((width + gap) / (CARD.x + gap)).floor() as usize).max(1);
                let used = cols as f32 * CARD.x + (cols - 1) as f32 * gap;
                let indent = ((width - used) / 2.0).max(0.0);
                for row in self.items.clone().chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        for entry in row {
                            if let Some(t) = self.card(ui, entry, &selected, actions) {
                                drop_target = Some(t);
                            }
                        }
                    });
                }
            });
        self.finish_drag(ui, drop_target, actions);
        if std::mem::take(&mut self.clear_requested) {
            self.clear();
        }
        // Let go of the file before it's opened elsewhere or deleted.
        if let Some(p) = &self.player
            && actions
                .iter()
                .any(|a| matches!(a, Action::Open(x) | Action::Delete(x) if *x == p.path))
        {
            self.stop_playback();
        }
    }

    /// The images dragged along with `path`: the whole Ctrl-click selection
    /// if it's part of it.
    fn dragged_with(&self, path: &Path) -> Vec<PathBuf> {
        if self.picked.len() > 1 && self.picked.iter().any(|p| p == path) {
            self.picked_images()
        } else {
            vec![path.to_owned()]
        }
    }

    fn picked_images(&self) -> Vec<PathBuf> {
        self.picked
            .iter()
            .filter(|p| thumbnail::kind(p) == Kind::Image)
            .cloned()
            .collect()
    }

    /// Draws one card. While another image is dragged over it, returns it
    /// and which way to combine them if dropped now (`None`: over the card
    /// but not on a button).
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        entry: &Entry,
        shown: &Path,
        actions: &mut Vec<Action>,
    ) -> Option<(PathBuf, Option<Direction>)> {
        let path = entry.path.as_path();
        let is_image = thumbnail::kind(path) == Kind::Image;
        let sense = if is_image {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let (rect, resp) = ui.allocate_exact_size(CARD, sense);
        if !ui.is_rect_visible(rect) {
            return None; // don't load thumbnails that are scrolled away
        }
        // With a Ctrl-click selection, only its cards are highlighted (and
        // numbered in the order they'll be combined).
        let order = self.picked.iter().position(|p| p == path);
        let selected = if self.picked.is_empty() {
            path == shown
        } else {
            order.is_some()
        };
        let visuals = ui.visuals().clone();
        ui.painter()
            .rect_filled(rect, 6.0, visuals.extreme_bg_color);
        match self.thumb(path, false) {
            Thumb::Ready { texture, .. } => {
                let tex = texture.size_vec2();
                let scale = ((rect.width() - 8.0) / tex.x).min((rect.height() - 8.0) / tex.y);
                let img_rect = egui::Rect::from_center_size(rect.center(), tex * scale);
                egui::Image::new((texture.id(), tex))
                    .corner_radius(3.0)
                    .paint_at(ui, img_rect);
                if thumbnail::kind(path) == Kind::Video {
                    thumbnail::draw_play_badge(ui.painter(), img_rect.center(), 14.0);
                }
            }
            Thumb::Loading => egui::Spinner::new().paint_at(
                ui,
                egui::Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            ),
            Thumb::Failed if thumbnail::kind(path) == Kind::Image => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "?",
                    egui::FontId::proportional(18.0),
                    visuals.weak_text_color(),
                );
            }
            Thumb::Failed => thumbnail::draw_file_icon(ui.painter(), rect.shrink(4.0), path),
        }
        let stroke = if selected {
            Stroke::new(2.0, ACCENT)
        } else if resp.hovered() {
            Stroke::new(1.0, visuals.widgets.hovered.bg_stroke.color)
        } else {
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        ui.painter()
            .rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
        if let Some(i) = order {
            let center = rect.left_top() + vec2(14.0, 14.0);
            ui.painter()
                .circle(center, 9.0, ACCENT, Stroke::new(1.5, Color32::WHITE));
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                (i + 1).to_string(),
                egui::FontId::proportional(11.0),
                Color32::WHITE,
            );
        }

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if resp.drag_started() {
            self.dragging = Some(path.to_owned());
        }
        let mut target = None;
        if let Some(dragged) = &self.dragging
            && is_image
            && !self.dragged_with(dragged).iter().any(|p| p == path)
            && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
            && rect.contains(pos)
            && ui.clip_rect().contains(pos)
        {
            target = Some((path.to_owned(), drop_zone(ui, rect, pos)));
        }
        let resp = if self.dragging.is_some() {
            resp
        } else {
            resp.on_hover_text(format!("{name}\n{}", details(path, None)))
        };
        if resp.clicked() {
            if ui.input(|i| i.modifiers.command) {
                // Ctrl-click: add to (or take out of) the selection, which
                // keeps the order the images were clicked in.
                if let Some(i) = self.picked.iter().position(|p| p == path) {
                    self.picked.remove(i);
                } else {
                    self.picked.push(path.to_owned());
                    self.selected = Some(path.to_owned());
                }
            } else {
                self.picked.clear();
                self.selected = Some(path.to_owned());
            }
        }
        if resp.double_clicked() {
            actions.push(Action::Open(path.to_owned()));
        }
        let combinable = if self.picked.iter().any(|p| p == path) {
            self.picked_images()
        } else {
            Vec::new()
        };
        resp.context_menu(|ui| {
            if combinable.len() > 1 {
                for (label, dir) in [
                    ("Combine horizontally", Direction::Horizontal),
                    ("Combine vertically", Direction::Vertical),
                ] {
                    if ui
                        .button(label)
                        .on_hover_text(format!(
                            "Stitch the {} selected images into a new screenshot",
                            combinable.len()
                        ))
                        .clicked()
                    {
                        actions.push(Action::Combine(combinable.clone(), dir));
                        ui.close();
                    }
                }
                ui.separator();
            }
            if let Some(link) = &entry.link
                && ui.button("Copy link").clicked()
            {
                actions.push(Action::CopyLink(link.clone()));
                ui.close();
            }
            if let Some(remote) = &entry.remote
                && ui
                    .button("Delete remotely")
                    .on_hover_text("Delete the uploaded copy; the file here is kept")
                    .clicked()
            {
                actions.push(Action::DeleteRemote(path.to_owned(), remote.clone()));
                ui.close();
            }
            for (label, action) in [
                ("Open", Action::Open(path.to_owned())),
                ("Copy", Action::Copy(path.to_owned())),
                ("Pin to screen", Action::Pin(path.to_owned())),
                ("Show in folder", Action::ShowInFolder(path.to_owned())),
                ("Delete", Action::Delete(path.to_owned())),
            ] {
                if matches!(label, "Copy" | "Pin to screen") && !is_image {
                    continue;
                }
                if ui.button(label).clicked() {
                    actions.push(action);
                    ui.close();
                }
            }
            ui.separator();
            self.clear_menu_item(ui);
        });
        target
    }

    /// Follows a card being dragged: a ghost of it under the pointer, and
    /// on release over a drop button, the combine action.
    fn finish_drag(
        &mut self,
        ui: &mut egui::Ui,
        target: Option<(PathBuf, Option<Direction>)>,
        actions: &mut Vec<Action>,
    ) {
        let Some(dragged) = self.dragging.clone() else {
            return;
        };
        let (down, pos) = ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
        if !down {
            self.dragging = None;
            if let Some((target, Some(dir))) = target {
                let mut paths = vec![target];
                paths.extend(self.dragged_with(&dragged));
                actions.push(Action::Combine(paths, dir));
            }
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let Some(pos) = pos else { return };
        let count = self.dragged_with(&dragged).len();
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("combine-drag"),
        ));
        if let Thumb::Ready { texture, .. } = self.thumb(&dragged, false) {
            let tex = texture.size_vec2();
            let size = tex * (96.0 / tex.x).min(64.0 / tex.y);
            let rect = egui::Rect::from_min_size(pos + vec2(12.0, 12.0), size);
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::from_white_alpha(190),
            );
            painter.rect_stroke(rect, 3.0, Stroke::new(1.0, ACCENT), StrokeKind::Outside);
            if count > 1 {
                painter.circle_filled(rect.right_top(), 10.0, ACCENT);
                painter.text(
                    rect.right_top(),
                    egui::Align2::CENTER_CENTER,
                    count.to_string(),
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        }
    }
}

/// Covers a card with *Horizontal* and *Vertical* drop buttons while an
/// image is dragged over it, and returns the one under the pointer.
fn drop_zone(ui: &egui::Ui, card: egui::Rect, pointer: egui::Pos2) -> Option<Direction> {
    let p = ui.painter();
    let visuals = ui.visuals();
    let shade = if visuals.dark_mode {
        Color32::from_black_alpha(215)
    } else {
        Color32::from_white_alpha(225)
    };
    p.rect_filled(card, 6.0, shade);
    p.rect_stroke(card, 6.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
    let text = visuals.strong_text_color();
    p.text(
        egui::pos2(card.center().x, card.top() + 13.0),
        egui::Align2::CENTER_CENTER,
        "Combine images",
        egui::FontId::proportional(13.0),
        text,
    );
    let area = egui::Rect::from_min_max(card.min + vec2(8.0, 26.0), card.max - vec2(8.0, 8.0));
    let half = (area.width() - 8.0) / 2.0;
    let mut hit = None;
    for (i, dir, label) in [
        (0.0, Direction::Horizontal, "Horizontal"),
        (1.0, Direction::Vertical, "Vertical"),
    ] {
        let r = egui::Rect::from_min_size(
            area.min + vec2(i * (half + 8.0), 0.0),
            vec2(half, area.height()),
        );
        let hovered = r.contains(pointer);
        if hovered {
            hit = Some(dir);
        }
        let fill = if hovered {
            ACCENT.gamma_multiply(0.35)
        } else {
            visuals.extreme_bg_color
        };
        p.rect_filled(r, 5.0, fill);
        let stroke = Stroke::new(if hovered { 2.0 } else { 1.0 }, ACCENT);
        p.rect_stroke(r, 5.0, stroke, StrokeKind::Inside);
        // A square split the way the images will be laid out.
        let icon = egui::Rect::from_center_size(r.center() - vec2(0.0, 8.0), vec2(16.0, 16.0));
        let s = Stroke::new(1.5, text);
        p.rect_stroke(icon, 2.0, s, StrokeKind::Inside);
        let (a, b) = match dir {
            Direction::Horizontal => (icon.center_top(), icon.center_bottom()),
            Direction::Vertical => (icon.left_center(), icon.right_center()),
        };
        p.line_segment([a, b], s);
        p.text(
            egui::pos2(r.center().x, icon.bottom() + 12.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.5),
            text,
        );
    }
    hit
}

fn empty_state(ui: &mut egui::Ui, hotkey: &str, actions: &mut Vec<Action>) {
    ui.add_space(ui.available_height() * 0.25);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No screenshots yet").heading());
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Press {hotkey} to capture a region.")).weak());
        ui.add_space(12.0);
        if ui
            .add(egui::Button::new(RichText::new("Capture now").strong()).fill(ACCENT))
            .clicked()
        {
            actions.push(Action::Capture);
        }
    });
}

/// The size of a `size` picture shrunk to fit `max`, as the loader does.
fn fit(size: [u32; 2], max: (u32, u32)) -> Vec2 {
    let [w, h] = size.map(|v| v.max(1) as f32);
    let scale = (max.0 as f32 / w).min(max.1 as f32 / h).min(1.0);
    vec2(w * scale, h * scale)
}

/// "1920 × 1080 · 1.2 MB · 5 min ago"
/// Play/pause, a seek bar and the time, along the bottom of the preview.
fn video_controls(
    ui: &mut egui::Ui,
    preview: egui::Rect,
    player: &mut Player,
    scrub: &mut Option<f32>,
) {
    let bar = egui::Rect::from_min_max(
        preview.left_bottom() + vec2(12.0, -44.0),
        preview.right_bottom() - vec2(12.0, 12.0),
    );
    ui.painter()
        .rect_filled(bar, 6.0, Color32::from_black_alpha(170));

    // Play / pause.
    let button = egui::Rect::from_min_size(bar.min, vec2(bar.height(), bar.height()));
    let resp = ui.interact(button, ui.id().with("video-play"), Sense::click());
    let c = button.center();
    let color = if resp.hovered() {
        Color32::WHITE
    } else {
        Color32::from_gray(210)
    };
    if player.is_paused() {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![c + vec2(-5.0, -7.0), c + vec2(8.0, 0.0), c + vec2(-5.0, 7.0)],
            color,
            Stroke::NONE,
        ));
    } else {
        for dx in [-5.0, 1.5] {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(c + vec2(dx, -7.0), vec2(3.5, 14.0)),
                1.0,
                color,
            );
        }
    }
    let tip = if player.is_paused() { "Play" } else { "Pause" };
    if resp.on_hover_text(tip).clicked() {
        player.toggle();
    }

    let Some(duration) = player.duration().filter(|d| *d > 0.0) else {
        return;
    };
    // The time, on the right.
    let at = scrub.map_or(player.position(), |f| f as f64 * duration);
    let galley = ui.painter().layout_no_wrap(
        format!("{} / {}", clock(at), clock(duration)),
        egui::FontId::monospace(12.0),
        Color32::from_gray(210),
    );
    let text_pos = egui::pos2(
        bar.right() - 10.0 - galley.size().x,
        bar.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(text_pos, galley, Color32::WHITE);

    // Seek bar: click or drag to jump. Dragging only moves the knob; the
    // video restarts from there on release.
    let track = egui::Rect::from_x_y_ranges(
        (button.right() + 4.0)..=(text_pos.x - 12.0),
        (bar.center().y - 8.0)..=(bar.center().y + 8.0),
    );
    if track.width() < 20.0 {
        return;
    }
    let resp = ui.interact(track, ui.id().with("video-seek"), Sense::click_and_drag());
    let fraction = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0);
    if resp.is_pointer_button_down_on()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        *scrub = Some(fraction(pos.x));
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(f) = scrub.take()
    {
        player.seek(f as f64 * duration);
    }
    let shown = scrub
        .unwrap_or((player.position() / duration) as f32)
        .clamp(0.0, 1.0);
    let line = egui::Rect::from_x_y_ranges(
        track.x_range(),
        (track.center().y - 2.0)..=(track.center().y + 2.0),
    );
    let played = egui::Rect::from_min_max(
        line.min,
        egui::pos2(line.left() + line.width() * shown, line.bottom()),
    );
    let painter = ui.painter();
    painter.rect_filled(line, 2.0, Color32::from_white_alpha(60));
    painter.rect_filled(played, 2.0, ACCENT);
    let knob = if resp.hovered() || scrub.is_some() { 6.0 } else { 4.5 };
    painter.circle_filled(egui::pos2(played.right(), line.center().y), knob, Color32::WHITE);
}

/// `m:ss`, or `h:mm:ss` for long videos.
fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn details(path: &Path, size: Option<[u32; 2]>) -> String {
    let mut parts = Vec::new();
    if let Some([w, h]) = size {
        parts.push(format!("{w} \u{00d7} {h}"));
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let bytes = meta.len() as f64;
        parts.push(if bytes >= 1024.0 * 1024.0 {
            format!("{:.1} MB", bytes / 1024.0 / 1024.0)
        } else {
            format!("{:.0} KB", (bytes / 1024.0).ceil())
        });
        if let Ok(modified) = meta.modified() {
            parts.push(ago(modified));
        }
    }
    parts.join(" \u{00b7} ")
}

fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "yesterday".into(),
        _ => chrono::DateTime::<chrono::Local>::from(t)
            .format("%-d %b %Y")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    /// Renders the drop buttons over a card, the left one under the pointer,
    /// to `target/combine-drop-preview.png`:
    /// `cargo test combine_drop_preview -- --ignored`.
    #[test]
    #[ignore]
    fn combine_drop_preview() {
        crate::preview::render("combine-drop-preview", [384, 256], 2.0, |root| {
            let full = root.max_rect();
            root.painter()
                .rect_filled(full, 0.0, Color32::from_rgb(27, 27, 30));
            let card = egui::Rect::from_center_size(full.center(), CARD);
            root.painter()
                .rect_filled(card, 6.0, root.visuals().extreme_bg_color);
            drop_zone(root, card, card.left_center() + vec2(40.0, 10.0));
        });
    }

    /// Three plain-colour images in a gallery, laid out at 900 × 640 so the
    /// cards' centres are at y = 477 and x = 96 (red), 282 (green), 468 (blue).
    fn test_gallery() -> Gallery {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ctrl-click");
        std::fs::create_dir_all(&dir).unwrap();
        let mut items = Vec::new();
        for (i, (w, h, c)) in [
            (400, 300, [200, 60, 60]),
            (300, 400, [60, 160, 60]),
            (500, 200, [60, 90, 220]),
        ]
        .into_iter()
        .enumerate()
        {
            let p = dir.join(format!("img{i}.png"));
            image::RgbaImage::from_pixel(w, h, image::Rgba([c[0], c[1], c[2], 255]))
                .save(&p)
                .unwrap();
            items.push(Entry::new(p));
        }
        let mut g = Gallery::new(&dir, String::new(), Box::new(|| {}));
        g.items = items;
        g
    }

    /// One frame's worth of input for each step of Ctrl-clicking each point.
    fn ctrl_clicks(points: &[egui::Pos2]) -> Vec<Vec<egui::Event>> {
        use egui::{Event, Modifiers, PointerButton};
        let ctrl = Modifiers::COMMAND | Modifiers::CTRL;
        let mut steps = vec![vec![Event::ModifiersChanged(ctrl)]];
        for &pos in points {
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: ctrl,
            };
            steps.push(vec![Event::PointerMoved(pos)]);
            steps.push(vec![button(true)]);
            steps.push(vec![button(false)]);
        }
        steps.push(vec![Event::ModifiersChanged(Modifiers::NONE)]);
        steps.push(vec![]);
        steps
    }

    fn gallery_ui(g: &mut Gallery) -> impl FnMut(&mut egui::Ui) + '_ {
        |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let mut actions = Vec::new();
                g.ui(ui, "Ctrl+Shift+S", &mut actions);
            });
        }
    }

    #[test]
    fn ctrl_click_keeps_click_order() {
        let mut g = test_gallery();
        let names = g.items.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        let ctx = egui::Context::default();
        let mut ui = gallery_ui(&mut g);
        let (red, green, blue) = (pos2(96.0, 477.0), pos2(282.0, 477.0), pos2(468.0, 477.0));
        // The newest (red) is shown, but isn't picked until it's clicked:
        // blue, then red, then green, out of grid order.
        for events in std::iter::once(vec![]).chain(ctrl_clicks(&[blue, red, green])) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(900.0, 640.0),
                )),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, &mut ui).textures_delta.clear();
        }
        drop(ui);
        assert_eq!(
            g.picked,
            vec![names[2].clone(), names[0].clone(), names[1].clone()]
        );
    }

    /// Ctrl-clicks cards, saving the last frame as `target/ctrl-click.png`.
    /// Set `CLICKS` to `x,y;x,y;...` (points, see `test_gallery`).
    #[test]
    #[ignore]
    fn ctrl_click_preview() {
        let mut g = test_gallery();
        let mut ui = gallery_ui(&mut g);
        let mut screen = crate::preview::Offscreen::new([900, 640], 1.0);
        for _ in 0..30 {
            screen.run(Vec::new(), &mut ui);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let clicks = std::env::var("CLICKS").unwrap_or_default();
        let points: Vec<_> = clicks
            .split(';')
            .filter_map(|p| p.split_once(','))
            .map(|(x, y)| pos2(x.parse().unwrap(), y.parse().unwrap()))
            .collect();
        for events in ctrl_clicks(&points) {
            screen.run(events, &mut ui);
        }
        screen.save("ctrl-click");
    }
}

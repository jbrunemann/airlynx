//! Browsing a favorite's save folder: list its folders and download files from it.
//!
//! The other device answers over the same encrypted TCP protocol as a send (see `airlynx_core::browse`), one
//! connection per folder listing and per download. Each runs on its own thread and writes into `State`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use airlynx_core::browse::{self, DirEntry};
use airlynx_core::{Identity, human_age, human_size};
use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

use crate::platform;
use crate::theme::{self, Icon, Kind, Tint, pal};

pub enum Action {
    None,
    Close,
}

#[derive(Clone)]
enum Listing {
    Loading,
    Ready(Vec<DirEntry>),
    Failed(String),
}

#[derive(Clone)]
enum Fetch {
    Idle,
    Running { name: String, received: u64, size: u64 },
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone)]
struct State {
    listing: Listing,
    fetch: Fetch,
    /// Counts folder requests, so an answer that arrives after the user moved on is dropped.
    generation: u64,
}

/// What the user did in one frame, applied after drawing.
enum Nav {
    Enter(String),
    Up,
    Refresh,
    Download(String, u64),
    DismissFetch,
}

pub struct RemoteBrowser {
    name: String,
    addr: SocketAddr,
    identity: Identity,
    /// Downloads land here, in this device's own save folder.
    save_dir: PathBuf,
    /// Folder names from the other device's save folder down to the folder being shown.
    path: Vec<String>,
    state: Arc<Mutex<State>>,
    /// Set when the folder changes so the next frame starts the list at the top.
    scroll_to_top: bool,
}

impl RemoteBrowser {
    pub fn new(ctx: &egui::Context, identity: Identity, addr: SocketAddr, name: String, save_dir: PathBuf) -> Self {
        let browser = Self {
            name,
            addr,
            identity,
            save_dir,
            path: Vec::new(),
            state: Arc::new(Mutex::new(State { listing: Listing::Loading, fetch: Fetch::Idle, generation: 0 })),
            scroll_to_top: true,
        };
        browser.load(ctx);
        browser
    }

    fn path_string(&self, file: Option<&str>) -> String {
        let mut parts: Vec<&str> = self.path.iter().map(String::as_str).collect();
        parts.extend(file);
        parts.join("/")
    }

    fn load(&self, ctx: &egui::Context) {
        let generation = {
            let mut s = self.state.lock().unwrap();
            s.generation += 1;
            s.listing = Listing::Loading;
            s.generation
        };
        let (state, ctx, identity, addr, path) =
            (self.state.clone(), ctx.clone(), self.identity.clone(), self.addr, self.path_string(None));
        thread::spawn(move || {
            let result = browse::list(addr, &identity, &path);
            let mut s = state.lock().unwrap();
            if s.generation == generation {
                s.listing = match result {
                    Ok(entries) => Listing::Ready(entries),
                    Err(e) => Listing::Failed(e.to_string()),
                };
            }
            drop(s);
            ctx.request_repaint();
        });
    }

    fn download(&self, ctx: &egui::Context, file: &str, size: u64) {
        if matches!(self.state.lock().unwrap().fetch, Fetch::Running { .. }) {
            return;
        }
        self.state.lock().unwrap().fetch = Fetch::Running { name: file.to_string(), received: 0, size };
        let (state, ctx, identity, addr, dir) =
            (self.state.clone(), ctx.clone(), self.identity.clone(), self.addr, self.save_dir.clone());
        let (path, name) = (self.path_string(Some(file)), file.to_string());
        thread::spawn(move || {
            let result = browse::download(addr, &identity, &path, &dir, |received, size| {
                state.lock().unwrap().fetch = Fetch::Running { name: name.clone(), received, size };
                ctx.request_repaint();
            });
            state.lock().unwrap().fetch = match result {
                Ok(saved) => Fetch::Done(saved),
                Err(e) => Fetch::Failed(e.to_string()),
            };
            ctx.request_repaint();
        });
    }

    fn navigate(&mut self, ctx: &egui::Context, nav: Nav) {
        match nav {
            Nav::Enter(dir) => {
                self.path.push(dir);
                self.scroll_to_top = true;
                self.load(ctx);
            }
            Nav::Up => {
                self.path.pop();
                self.scroll_to_top = true;
                self.load(ctx);
            }
            Nav::Refresh => self.load(ctx),
            Nav::Download(file, size) => self.download(ctx, &file, size),
            Nav::DismissFetch => {
                let mut s = self.state.lock().unwrap();
                if !matches!(s.fetch, Fetch::Running { .. }) {
                    s.fetch = Fetch::Idle;
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> Action {
        let p = pal(ui);
        let mut action = Action::None;
        let mut nav = None;
        let state = self.state.lock().unwrap().clone();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());

        ui.horizontal(|ui| {
            if theme::button(ui, Kind::Secondary, None, "Back", true).clicked() {
                action = Action::Close;
            }
            ui.add(egui::Label::new(RichText::new(format!("Files on {}", self.name)).family(theme::bold()).size(16.0)).truncate());
        });
        ui.horizontal(|ui| {
            if theme::button(ui, Kind::Secondary, None, "Up", !self.path.is_empty()).clicked() {
                nav = Some(Nav::Up);
            }
            if theme::icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                nav = Some(Nav::Refresh);
            }
            ui.add(egui::Label::new(RichText::new(format!("/{}", self.path_string(None))).monospace()).truncate());
        });

        match &state.fetch {
            Fetch::Idle => {}
            Fetch::Running { name, received, size } => {
                theme::card(ui, Some(p.accent), |ui| {
                    let fraction = if *size == 0 { 1.0 } else { *received as f32 / *size as f32 };
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(format!("Downloading {name}")).family(theme::bold())).truncate());
                    });
                    theme::progress(ui, fraction);
                    theme::sub(ui, &format!("{} of {}", human_size(*received), human_size(*size)));
                });
            }
            Fetch::Done(path) => {
                theme::banner(ui, p.ok_soft, |ui| {
                    let saved = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(format!("Saved {saved} to your save folder")).color(p.ok));
                    ui.horizontal(|ui| {
                        if theme::button(ui, Kind::Text, None, "Open", true).clicked()
                            && let Err(e) = platform::open_file(path)
                        {
                            self.state.lock().unwrap().fetch = Fetch::Failed(e);
                        }
                        if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                            nav = Some(Nav::DismissFetch);
                        }
                    });
                });
            }
            Fetch::Failed(error) => {
                theme::banner(ui, p.err_soft, |ui| {
                    ui.label(RichText::new(format!("Download failed: {error}")).color(p.err));
                    if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                        nav = Some(Nav::DismissFetch);
                    }
                });
            }
        }
        ui.add_space(4.0);

        let downloading = matches!(state.fetch, Fetch::Running { .. });
        let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
        if std::mem::take(&mut self.scroll_to_top) {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| match &state.listing {
            Listing::Loading => {
                ui.label(RichText::new("Loading...").size(13.0).color(p.sub));
            }
            Listing::Failed(error) => {
                theme::banner(ui, p.err_soft, |ui| {
                    ui.label(RichText::new(format!("Could not list this folder: {error}")).color(p.err));
                });
            }
            Listing::Ready(entries) if entries.is_empty() => {
                ui.label(RichText::new("This folder is empty.").size(13.0).color(p.sub));
            }
            Listing::Ready(entries) => {
                theme::card(ui, None, |ui| {
                    for (i, entry) in entries.iter().enumerate() {
                        if i > 0 {
                            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                            ui.painter().rect_filled(line, 0.0, p.line);
                        }
                        let row = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            theme::tile(ui, if entry.is_dir { Icon::Folder } else { Icon::File }, 36.0, Tint::Accent);
                            let button_w = if entry.is_dir { 0.0 } else { 100.0 };
                            let mid_w = (ui.available_width() - button_w - 12.0).max(40.0);
                            ui.allocate_ui_with_layout(vec2(mid_w, 36.0), Layout::top_down(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add(egui::Label::new(RichText::new(&entry.name).size(14.5)).truncate());
                                let age = if entry.modified == 0 { String::new() } else { human_age(now.saturating_sub(entry.modified)) };
                                let detail = match (entry.is_dir, age.is_empty()) {
                                    (true, true) => String::new(),
                                    (true, false) => age,
                                    (false, true) => human_size(entry.size),
                                    (false, false) => format!("{}, {age}", human_size(entry.size)),
                                };
                                ui.add(egui::Label::new(RichText::new(detail).small().color(p.sub)).truncate());
                            });
                            if !entry.is_dir {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::button(ui, Kind::Text, Some(Icon::Download), "Get", !downloading).clicked() {
                                        nav = Some(Nav::Download(entry.name.clone(), entry.size));
                                    }
                                });
                            }
                        });
                        if entry.is_dir {
                            let tap = ui.interact(row.response.rect, row.response.id.with("open"), Sense::click());
                            if tap.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                nav = Some(Nav::Enter(entry.name.clone()));
                            }
                        }
                    }
                });
            }
        });

        if let Some(nav) = nav {
            self.navigate(ctx, nav);
        }
        action
    }
}

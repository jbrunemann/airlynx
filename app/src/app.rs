use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use airlynx_core::receive::{self, ReceiveEvent, ReceiverConfig, ReceiverHandle};
use airlynx_core::send::{self, Entry, Progress, Report};
use airlynx_core::{DEFAULT_TCP_PORT, DeviceId, DeviceKind, Favorites, Identity, Peer, Profile, human_size, scan};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, vec2};

use crate::platform;
use crate::remote::{self, RemoteBrowser};
use crate::theme::{self, Icon, Kind, Tint, pal};

/// How often the network is scanned in the background while the window has focus and shows which devices
/// are online (the Send and Settings pages).
const SCAN_INTERVAL: Duration = Duration::from_secs(6);
/// How often otherwise. Each scan wakes every device that answers, so this is long.
const IDLE_SCAN_INTERVAL: Duration = Duration::from_secs(60);
/// A device counts as online until it has been silent this long, so one lost
/// UDP packet does not make a favorite flicker offline.
const PEER_TTL: Duration = Duration::from_secs(20);
/// Windows at least this wide get a sidebar and two columns, narrower ones a tab bar.
const WIDE_WIDTH: f32 = 720.0;
const BADGE_COLOR: Color32 = Color32::from_rgb(0xC6, 0x28, 0x28);

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    Send,
    Received,
    Settings,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Send, Tab::Received, Tab::Settings];

    fn label(self) -> &'static str {
        match self {
            Tab::Send => "Send",
            Tab::Received => "Received",
            Tab::Settings => "Settings",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Send => "Send files",
            Tab::Received => "Received",
            Tab::Settings => "Settings",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Tab::Send => Icon::Send,
            Tab::Received => Icon::Inbox,
            Tab::Settings => Icon::Sliders,
        }
    }
}

#[derive(Clone, Default)]
enum SendStatus {
    #[default]
    Idle,
    Sending(Progress),
    Done(Report),
    Failed(String),
}

/// An in-flight incoming file; one per connected sender.
#[derive(Clone)]
struct Incoming {
    peer: String,
    from: String,
    name: String,
    received: u64,
    size: u64,
}

/// One finished (or failed) incoming file, newest last.
#[derive(Clone)]
struct Received {
    ok: bool,
    name: String,
    size: u64,
    from: String,
    /// Why it failed; empty when `ok`.
    note: String,
    path: Option<PathBuf>,
    at: Instant,
}

#[derive(Clone)]
struct SeenPeer {
    peer: Peer,
    seen: Instant,
}

/// A device that tried to send but is not a favorite.
#[derive(Clone)]
struct Request {
    id: DeviceId,
    name: String,
}

/// State written by background threads and read by the UI.
#[derive(Clone, Default)]
struct Shared {
    scanning: bool,
    /// The scan in flight was started by the user, so show it. Background scans are silent.
    scan_visible: bool,
    scanned_once: bool,
    peers: Vec<SeenPeer>,
    scan_error: Option<String>,
    send: SendStatus,
    send_to: String,
    send_target: Option<DeviceId>,
    send_started: Option<Instant>,
    /// Files the receiver confirmed; the UI drops them from the send list.
    delivered: Vec<PathBuf>,
    incoming: Vec<Incoming>,
    requests: Vec<Request>,
    received: Vec<Received>,
}

impl Shared {
    fn online(&self, id: DeviceId) -> Option<&Peer> {
        self.peers
            .iter()
            .find(|p| p.peer.id == id && p.seen.elapsed() < PEER_TTL)
            .map(|p| &p.peer)
    }

    /// The name of the device behind an "ip:port" connection, if discovery has seen it.
    fn name_for(&self, peer: &str) -> String {
        let ip = peer.rsplit_once(':').map_or(peer, |(ip, _)| ip).trim_matches(['[', ']']);
        self.peers
            .iter()
            .find(|p| p.peer.addr.ip().to_string() == ip)
            .map_or_else(|| ip.to_string(), |p| p.peer.name.clone())
    }
}

/// What the user asked for during one frame. Applied after drawing so the
/// widgets never borrow the app mutably.
#[derive(Default)]
struct Actions {
    send_to: Option<(SocketAddr, String, DeviceId)>,
    browse: Option<(SocketAddr, String)>,
    add_favorite: Option<(DeviceId, String, DeviceKind)>,
    remove_favorite: Option<DeviceId>,
    dismiss_request: Option<DeviceId>,
    dismiss_send: bool,
    /// A new device name; empty goes back to the platform's name.
    rename: Option<String>,
    scan: bool,
}

pub struct AirLynxApp {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    identity: Identity,
    favorites: Favorites,
    notice: Option<String>,
    tab: Tab,
    /// How many received files the user has already looked at, for the tab badge.
    seen_received: usize,
    files: Vec<Entry>,
    file_error: Option<String>,
    /// Files from other apps (Android share target) still being copied.
    sharing: usize,
    receive_dir: PathBuf,
    receiver: Option<ReceiverHandle>,
    receiver_error: Option<String>,
    /// Why the last Open on the Received page did not work.
    open_error: Option<String>,
    /// A favorite's save folder, open in place of the pages.
    remote: Option<RemoteBrowser>,
    /// The favorite whose details are open in Settings.
    info_open: Option<DeviceId>,
    /// The device name being typed in Settings, while the editor is open.
    name_edit: Option<String>,
    last_scan: Instant,
    #[cfg(target_os = "android")]
    last_dir_check: Instant,
    #[cfg(target_os = "android")]
    browser: Option<crate::browser::FileBrowser>,
    #[cfg(target_os = "android")]
    diag: crate::diag::Diagnostics,
}

impl AirLynxApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        if platform::is_tv() {
            // Read from the couch: everything a little bigger.
            cc.egui_ctx.set_zoom_factor(1.4);
        }
        #[cfg(target_os = "android")]
        sync_theme(&cc.egui_ctx);

        let (profile, notice) = match Profile::load(&platform::config_dir()) {
            Ok(profile) => (profile, None),
            // Still usable for this session, but favorites will be lost on exit.
            Err(e) => (
                Profile { id: DeviceId::random(), name: None, favorites: Favorites::in_memory() },
                Some(format!("Favorites cannot be saved: {e}")),
            ),
        };
        let device_name = profile.name.clone().unwrap_or_else(platform::device_name);
        let mut app = Self {
            shared: Arc::default(),
            cancel: Arc::default(),
            identity: Identity::new(profile.id, &device_name).with_kind(platform::device_kind()),
            favorites: profile.favorites,
            notice,
            tab: Tab::default(),
            seen_received: 0,
            files: Vec::new(),
            file_error: None,
            sharing: 0,
            receive_dir: platform::default_receive_dir(),
            receiver: None,
            receiver_error: None,
            open_error: None,
            remote: None,
            info_open: None,
            name_edit: None,
            last_scan: Instant::now(),
            #[cfg(target_os = "android")]
            last_dir_check: Instant::now(),
            #[cfg(target_os = "android")]
            browser: None,
            #[cfg(target_os = "android")]
            diag: crate::diag::Diagnostics::new(&platform::default_receive_dir()),
        };
        app.start_receiver(&cc.egui_ctx);
        app.start_scan(&cc.egui_ctx, true);
        app
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let paths = paths
            .into_iter()
            .filter(|p| !self.files.iter().any(|e| &e.path == p));
        match send::prepare(paths) {
            Ok(entries) => {
                self.files.extend(entries);
                self.file_error = None;
            }
            Err(e) => self.file_error = Some(e.to_string()),
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context, visible: bool) {
        let shared = self.shared.clone();
        let favorites = self.favorites.clone();
        let identity = self.identity.clone();
        let ctx = ctx.clone();
        {
            let mut s = shared.lock().unwrap();
            s.scanning = true;
            s.scan_visible = visible;
        }
        self.last_scan = Instant::now();
        thread::spawn(move || {
            let result = scan(&identity, &favorites, Duration::from_millis(1500));
            let mut s = shared.lock().unwrap();
            s.scanning = false;
            s.scan_visible = false;
            s.scanned_once = true;
            match result {
                Ok(found) => {
                    let now = Instant::now();
                    for peer in found {
                        // Keep stored favorite names and kinds current.
                        if favorites.contains(peer.id) {
                            let _ = favorites.add(peer.id, &peer.name, peer.kind);
                        }
                        s.peers.retain(|p| p.peer.id != peer.id);
                        s.peers.push(SeenPeer { peer, seen: now });
                    }
                    s.peers.retain(|p| p.seen.elapsed() < PEER_TTL);
                    s.scan_error = None;
                }
                Err(e) => s.scan_error = Some(e.to_string()),
            }
            ctx.request_repaint();
        });
    }

    fn start_send(&mut self, ctx: &egui::Context, addr: SocketAddr, target_name: String, target: DeviceId) {
        let entries = self.files.clone();
        let shared = self.shared.clone();
        let cancel = self.cancel.clone();
        let identity = self.identity.clone();
        let ctx = ctx.clone();
        cancel.store(false, Ordering::Relaxed);
        {
            let mut s = shared.lock().unwrap();
            s.send_to = target_name;
            s.send_target = Some(target);
            s.send_started = Some(Instant::now());
            s.send = SendStatus::Sending(Progress {
                file_index: 0,
                file_count: entries.len(),
                file_name: String::new(),
                file_sent: 0,
                file_size: 0,
                total_sent: 0,
                total_size: entries.iter().map(|e| e.size).sum(),
            });
        }

        thread::spawn(move || {
            let result = send::send_files(addr, &identity, &entries, |p| {
                shared.lock().unwrap().send = SendStatus::Sending(p.clone());
                ctx.request_repaint();
                if cancel.load(Ordering::Relaxed) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
            let mut s = shared.lock().unwrap();
            s.send = match result {
                Ok(report) => {
                    s.delivered.extend(report.sent_paths.iter().cloned());
                    SendStatus::Done(report)
                }
                Err(e) => SendStatus::Failed(e.to_string()),
            };
            drop(s);
            ctx.request_repaint();
        });
    }

    /// The receiver is always on: favorites can send at any time.
    fn start_receiver(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "android")]
        {
            self.receive_dir = platform::default_receive_dir();
            if !self.diag.experiments().receiver {
                self.receiver_error = Some("Switched off under Diagnostics".into());
                return;
            }
        }
        let config = ReceiverConfig {
            dir: self.receive_dir.clone(),
            port: DEFAULT_TCP_PORT,
            identity: self.identity.clone(),
            favorites: self.favorites.clone(),
            // A phone is the device that tends to sit behind NAT or a VPN, so it opens the connections.
            reverse: cfg!(target_os = "android"),
        };
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        let result = receive::start(config, move |event| {
            handle_receive_event(&mut shared.lock().unwrap(), event);
            ctx.request_repaint();
        });
        match result {
            Ok(handle) => {
                self.receiver = Some(handle);
                platform::set_keep_awake(true);
                self.receiver_error = None;
            }
            Err(e) => self.receiver_error = Some(format!("Could not start receiver: {e}")),
        }
    }

    fn restart_receiver(&mut self, ctx: &egui::Context) {
        // Dropping the handle stops the listener. Transfers in progress finish.
        self.receiver = None;
        self.start_receiver(ctx);
    }

    /// Background upkeep: periodic scans, and on Android picking up a changed save folder
    /// once the user has granted file access, and a changed light/dark setting.
    /// The remote of an Android TV: egui moves focus from the focused widget, so the first arrow press (with nothing
    /// focused yet) takes the first widget, and Back steps out of a view, then to the Send page, then leaves the app.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        theme::track_input(ctx);
        let (arrow, back) = ctx.input(|i| {
            let arrow = [egui::Key::ArrowUp, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowRight].into_iter().any(|k| i.key_pressed(k));
            (arrow, i.key_pressed(egui::Key::BrowserBack))
        });
        if arrow && ctx.memory(|m| m.focused()).is_none() {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::Next));
        }
        if !back {
            return;
        }
        #[cfg(target_os = "android")]
        if self.browser.take().is_some() {
            return;
        }
        if self.remote.take().is_some() {
            return;
        }
        if self.tab == Tab::Send {
            platform::go_home();
        } else {
            self.tab = Tab::Send;
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let delivered = std::mem::take(&mut self.shared.lock().unwrap().delivered);
        if !delivered.is_empty() {
            self.files.retain(|e| !delivered.contains(&e.path));
            delivered.iter().for_each(|path| platform::discard_copy(path));
        }
        #[cfg(target_os = "android")]
        self.take_shared_files();
        // Every scan wakes the other devices (a phone's radio and CPU included), so scan often only while
        // someone is looking at which devices are online.
        let watching = ctx.input(|i| i.focused) && matches!(self.tab, Tab::Send | Tab::Settings) && !self.browsing();
        let interval = if watching { SCAN_INTERVAL } else { IDLE_SCAN_INTERVAL };
        let scanning = self.shared.lock().unwrap().scanning;
        if !scanning && self.last_scan.elapsed() >= interval {
            self.start_scan(ctx, false);
        }
        #[cfg(target_os = "android")]
        if self.last_dir_check.elapsed() >= Duration::from_secs(2) {
            self.last_dir_check = Instant::now();
            sync_theme(ctx);
            if self.receiver.is_some() && platform::default_receive_dir() != self.receive_dir {
                self.restart_receiver(ctx);
            }
        }
        // Shared files appear as soon as they are copied, so look more often while that runs.
        ctx.request_repaint_after(if self.sharing > 0 { Duration::from_millis(250) } else { Duration::from_secs(1) });
    }

    /// Files other apps shared to AirLynx join the send list, and the Send page comes to the front.
    #[cfg(target_os = "android")]
    fn take_shared_files(&mut self) {
        let shared = platform::take_shared();
        self.sharing = shared.preparing;
        if let Some(error) = shared.error {
            self.file_error = Some(error);
        }
        if !shared.ready.is_empty() {
            self.add_files(shared.ready);
            self.tab = Tab::Send;
            self.browser = None;
        }
    }

    /// The file browsers replace the whole screen, so the tabs are hidden while one is open.
    fn browsing(&self) -> bool {
        #[cfg(target_os = "android")]
        if self.browser.is_some() {
            return true;
        }
        self.remote.is_some()
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Actions) {
        if let Some((addr, name, id)) = actions.send_to {
            self.start_send(ctx, addr, name, id);
        }
        if let Some((addr, name)) = actions.browse {
            let (identity, dir) = (self.identity.clone(), self.receive_dir.clone());
            self.remote = Some(RemoteBrowser::new(ctx, identity, addr, name, dir));
        }
        if let Some((id, name, kind)) = actions.add_favorite {
            self.set_notice(self.favorites.add(id, &name, kind));
            self.shared.lock().unwrap().requests.retain(|r| r.id != id);
        }
        if let Some(id) = actions.remove_favorite {
            self.set_notice(self.favorites.remove(id));
        }
        if let Some(id) = actions.dismiss_request {
            self.shared.lock().unwrap().requests.retain(|r| r.id != id);
        }
        if actions.dismiss_send {
            self.shared.lock().unwrap().send = SendStatus::Idle;
        }
        if let Some(name) = actions.rename {
            self.rename(ctx, &name);
        }
        if actions.scan {
            let mut s = self.shared.lock().unwrap();
            if s.scanning {
                // A background scan is already running; just start showing it.
                s.scan_visible = true;
            } else {
                drop(s);
                self.start_scan(ctx, true);
            }
        }
    }

    /// Saves the new name and announces it: the receiver and the scans copy the identity, so the receiver restarts.
    fn rename(&mut self, ctx: &egui::Context, name: &str) {
        match Profile::save_name(&platform::config_dir(), name) {
            Ok(saved) => {
                self.identity.name = Identity::new(self.identity.id, &saved.unwrap_or_else(platform::device_name)).name;
                self.name_edit = None;
                self.notice = None;
                if self.receiver.is_some() {
                    self.restart_receiver(ctx);
                }
            }
            Err(e) => self.notice = Some(format!("The name cannot be saved: {e}")),
        }
    }

    fn set_notice(&mut self, result: std::io::Result<()>) {
        self.notice = result.err().map(|e| format!("Favorites cannot be saved: {e}"));
    }

    fn unseen(&self, s: &Shared) -> usize {
        s.received.len().saturating_sub(self.seen_received) + s.requests.len()
    }

    // ---- chrome -------------------------------------------------------------------------------

    fn receiving_pill(&self, ui: &mut egui::Ui) {
        let p = pal(ui);
        if self.receiver.is_some() {
            theme::pill(ui, p.ok_soft, p.ok, p.ok, "Ready to receive");
        } else {
            theme::pill(ui, p.err_soft, p.err, p.err, "Not receiving");
        }
    }

    /// Phone layout: logo and status on top.
    fn header(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = pal(ui);
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(16, 8)))
            .show_separator_line(false)
            .show(ui, |ui| {
                #[cfg(target_os = "android")]
                ui.add_space(platform::status_bar_height_px() / ctx.pixels_per_point());
                #[cfg(not(target_os = "android"))]
                let _ = ctx;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    theme::logo(ui, 34.0);
                    ui.label(RichText::new("AirLynx").family(theme::bold()).size(20.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.receiving_pill(ui));
                });
            });
    }

    /// Phone layout: three tabs along the bottom.
    fn tab_bar(&mut self, ui: &mut egui::Ui, unseen: usize) {
        let p = pal(ui);
        #[cfg(target_os = "android")]
        let inset = platform::navigation_bar_height_px() / ui.ctx().pixels_per_point();
        #[cfg(not(target_os = "android"))]
        let inset = 0.0;
        egui::Panel::bottom("tabs")
            .exact_size(72.0 + inset)
            .frame(egui::Frame::new().fill(p.surface))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let width = rect.width() / Tab::ALL.len() as f32;
                for (i, tab) in Tab::ALL.into_iter().enumerate() {
                    let cell = egui::Rect::from_min_size(rect.min + vec2(width * i as f32, 0.0), vec2(width, 72.0));
                    let response = ui.interact(cell, ui.scope_id().with(("tab", i)), Sense::click());
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tab.label()));
                    let active = self.tab == tab;
                    let color = if active { p.accent_text } else { p.sub };
                    let pill = egui::Rect::from_center_size(egui::pos2(cell.center().x, cell.top() + 25.0), vec2(56.0, 30.0));
                    if active {
                        ui.painter().rect_filled(pill, egui::CornerRadius::same(15), p.accent_soft);
                    }
                    tab.icon().paint(ui.painter(), egui::Rect::from_center_size(pill.center(), egui::Vec2::splat(22.0)), color, 1.9);
                    let font = egui::FontId::new(13.0, if active { theme::bold() } else { egui::FontFamily::Proportional });
                    theme::text_at(ui, egui::pos2(cell.center().x, cell.top() + 54.0), egui::Align2::CENTER_CENTER, tab.label(), font, color);
                    if tab == Tab::Received && unseen > 0 {
                        badge(ui, egui::pos2(pill.right() - 6.0, pill.top() + 1.0), unseen);
                    }
                    theme::focus_ring(ui, &response, 14);
                    if response.clicked() {
                        self.tab = tab;
                    }
                }
            });
    }

    /// Desktop layout: logo, navigation and receive status on the left.
    fn sidebar(&mut self, ui: &mut egui::Ui, unseen: usize) {
        let p = pal(ui);
        egui::Panel::left("nav")
            .exact_size(200.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.surface).inner_margin(egui::Margin::symmetric(12, 20)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.add_space(8.0);
                    theme::logo(ui, 34.0);
                    ui.label(RichText::new("AirLynx").family(theme::bold()).size(20.0));
                });
                ui.add_space(14.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                for tab in Tab::ALL {
                    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tab.label()));
                    let active = self.tab == tab;
                    let color = if active { p.accent_text } else { p.sub };
                    if active || response.hovered() {
                        let fill = if active { p.accent_soft } else { p.bg };
                        ui.painter().rect_filled(rect, egui::CornerRadius::same(10), fill);
                    }
                    tab.icon().paint(ui.painter(), egui::Rect::from_center_size(rect.left_center() + vec2(22.0, 0.0), egui::Vec2::splat(20.0)), color, 1.9);
                    let font = egui::FontId::new(15.0, if active { theme::bold() } else { egui::FontFamily::Proportional });
                    theme::text_at(ui, rect.left_center() + vec2(42.0, 0.0), egui::Align2::LEFT_CENTER, tab.label(), font, color);
                    if tab == Tab::Received && unseen > 0 {
                        badge(ui, rect.right_center() - vec2(20.0, 0.0), unseen);
                    }
                    theme::focus_ring(ui, &response, 10);
                    if response.clicked() {
                        self.tab = tab;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    egui::Frame::new()
                        .fill(p.bg)
                        .corner_radius(12)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            let (dot, text, label) = if self.receiver.is_some() {
                                (p.ok, p.ok, "Ready to receive")
                            } else {
                                (p.err, p.err, "Not receiving")
                            };
                            theme::status_line(ui, dot, text, label);
                            ui.add(egui::Label::new(RichText::new(format!("Visible as {}", self.identity.name)).small().color(p.sub)).truncate());
                        });
                });
            });
    }

    // ---- send page ----------------------------------------------------------------------------

    fn send_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &Shared, wide: bool, actions: &mut Actions) {
        let sending = matches!(s.send, SendStatus::Sending(_));
        send_result(ui, s, actions);
        if wide {
            ui.spacing_mut().item_spacing.x = 24.0;
            ui.columns(2, |cols| {
                for col in cols.iter_mut() {
                    col.spacing_mut().item_spacing.x = 8.0;
                }
                self.files_section(&mut cols[0], ctx, s, sending);
                self.devices_section(&mut cols[1], s, sending, actions);
            });
        } else {
            self.files_section(ui, ctx, s, sending);
            self.devices_section(ui, s, sending, actions);
        }
    }

    fn files_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &Shared, sending: bool) {
        let p = pal(ui);
        let total: u64 = self.files.iter().map(|e| e.size).sum();
        let summary = (!self.files.is_empty())
            .then(|| format!("{} file{}, {}", self.files.len(), plural(self.files.len()), human_size(total)));
        theme::section(ui, "Files", summary.as_deref());

        if !self.files.is_empty() {
            let progress_index = match &s.send {
                SendStatus::Sending(p) => Some(p.file_index),
                _ => None,
            };
            let mut remove = None;
            theme::card(ui, None, |ui| {
                for (i, entry) in self.files.iter().enumerate() {
                    if i > 0 {
                        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                        ui.painter().rect_filled(line, 0.0, p.line);
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        theme::tile(ui, Icon::File, 36.0, Tint::Accent);
                        let right_w = 76.0;
                        let mid_w = (ui.available_width() - right_w - 12.0).max(40.0);
                        ui.allocate_ui_with_layout(vec2(mid_w, 36.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add(egui::Label::new(RichText::new(&entry.name).size(14.5)).truncate());
                            theme::sub(ui, &human_size(entry.size));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match progress_index {
                            None => {
                                if theme::icon_button(ui, Icon::Close, &format!("Remove {}", entry.name)).clicked() {
                                    remove = Some(i);
                                }
                            }
                            Some(current) => {
                                let (text, color) = match i.cmp(&current) {
                                    std::cmp::Ordering::Less => ("Sent", p.ok),
                                    std::cmp::Ordering::Equal => ("Sending", p.accent_text),
                                    std::cmp::Ordering::Greater => ("Waiting", p.sub),
                                };
                                ui.label(RichText::new(text).size(13.0).family(theme::bold()).color(color));
                            }
                        });
                    });
                }
            });
            if let Some(i) = remove {
                platform::discard_copy(&self.files.remove(i).path);
            }
        }

        if self.sharing > 0 {
            theme::sub(ui, &format!("Preparing {} shared file{}...", self.sharing, plural(self.sharing)));
        }
        if let Some(err) = &self.file_error {
            ui.label(RichText::new(err).color(p.err));
        }

        #[cfg(target_os = "android")]
        {
            let _ = ctx;
            if platform::has_all_files_access() {
                self.file_actions(ui, sending, |app| {
                    app.browser = Some(crate::browser::FileBrowser::new(platform::storage_root()));
                });
            } else {
                theme::card(ui, None, |ui| {
                    ui.label(RichText::new("AirLynx needs access to your files to send them.").color(p.sub));
                    if theme::button(ui, Kind::Primary, None, "Grant file access", true).clicked() {
                        platform::request_all_files_access();
                    }
                });
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
            let color = if hovering { p.accent_text } else { p.off };
            let zone = ui.vertical_centered(|ui| {
                ui.set_width(ui.available_width());
                ui.add_space(12.0);
                ui.label(RichText::new(if hovering { "Release to add" } else { "Drop files here" }).color(p.sub));
                if theme::button(ui, Kind::Secondary, Some(Icon::Plus), "Add files", !sending).clicked() {
                    self.add_files(platform::pick_files());
                }
                ui.add_space(12.0);
            });
            theme::dashed_rect(ui, zone.response.rect, color);
            if self.files.len() > 1 && !sending {
                self.file_actions(ui, sending, |_| {});
            }
        }
    }

    /// The row under the file list: on Android the browse button, plus Clear all.
    fn file_actions(&mut self, ui: &mut egui::Ui, sending: bool, browse: impl FnOnce(&mut Self)) {
        ui.horizontal(|ui| {
            #[cfg(target_os = "android")]
            if theme::button(ui, Kind::Secondary, Some(Icon::Plus), "Browse files", !sending).clicked() {
                browse(self);
            }
            #[cfg(not(target_os = "android"))]
            let _ = browse;
            if self.files.len() > 1 && theme::button(ui, Kind::Text, None, "Clear all", !sending).clicked() {
                self.files.drain(..).for_each(|e| platform::discard_copy(&e.path));
            }
        });
    }

    fn devices_section(&self, ui: &mut egui::Ui, s: &Shared, sending: bool, actions: &mut Actions) {
        let p = pal(ui);
        theme::section(ui, "Send to", Some("Favorites"));
        let favorites = self.favorites.list();
        if favorites.is_empty() {
            ui.label(
                RichText::new(
                    "No favorites yet. Add a device below. Once both devices have added each other, \
                     they can send files to each other any time.",
                )
                .size(13.0)
                .color(p.sub),
            );
        } else if self.files.is_empty() {
            theme::sub(ui, "Choose files, then press Send next to a device.");
        }
        for fav in &favorites {
            self.device_card(ui, s, fav, sending, actions);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Add a device").family(theme::bold()).size(16.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if s.scan_visible {
                    ui.add_space(10.0);
                    ui.spinner();
                } else if theme::icon_button(ui, Icon::Refresh, "Scan again").clicked() {
                    actions.scan = true;
                }
            });
        });
        if let Some(err) = &s.scan_error {
            ui.label(RichText::new(format!("Scan failed: {err}")).color(p.err));
        }
        let mut any = false;
        for seen in &s.peers {
            let peer = &seen.peer;
            if seen.seen.elapsed() >= PEER_TTL || self.favorites.contains(peer.id) {
                continue;
            }
            any = true;
            theme::card(ui, None, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    theme::tile(ui, device_icon(peer.kind), 40.0, Tint::Muted);
                    let mid_w = (ui.available_width() - 70.0 - 12.0).max(40.0);
                    ui.allocate_ui_with_layout(vec2(mid_w, 40.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(RichText::new(&peer.name).family(theme::bold()).size(15.0)).truncate());
                        theme::sub(ui, if peer.mutual { "Nearby, faved you" } else { "Nearby" });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::button(ui, Kind::Secondary, None, "Add", true).clicked() {
                            actions.add_favorite = Some((peer.id, peer.name.clone(), peer.kind));
                        }
                    });
                });
            });
        }
        if !any && s.scanned_once && !s.scan_visible {
            ui.label(RichText::new("No new devices found. Is AirLynx open on the other device?").size(13.0).color(p.sub));
        }
    }

    fn device_card(&self, ui: &mut egui::Ui, s: &Shared, fav: &airlynx_core::Favorite, sending: bool, actions: &mut Actions) {
        let p = pal(ui);
        let live = s.online(fav.id);
        let kind = live.map_or(fav.kind, |peer| peer.kind);
        let target = match &s.send {
            SendStatus::Sending(progress) if s.send_target == Some(fav.id) => Some(progress),
            _ => None,
        };
        let border = target.map(|_| p.accent);
        theme::card(ui, border, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                theme::tile(ui, device_icon(kind), 44.0, if live.is_some() { Tint::Accent } else { Tint::Muted });
                let can_browse = target.is_none() && live.is_some_and(|peer| peer.mutual);
                // The name gets the full width next to the tile. The status and the buttons share the row below.
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let name_color = if live.is_some() { p.text } else { p.sub };
                    ui.add(egui::Label::new(RichText::new(&fav.name).family(theme::bold()).size(15.5).color(name_color)).truncate());
                    ui.horizontal(|ui| {
                        ui.set_min_height(theme::control_height());
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if let Some(progress) = target {
                            ui.label(
                                RichText::new(format!("Sending, file {} of {}", progress.file_index + 1, progress.file_count))
                                    .size(13.0)
                                    .color(p.accent_text),
                            );
                        } else {
                            match live {
                                None => theme::status_line(ui, p.off, p.sub, "Offline"),
                                Some(peer) if peer.mutual => theme::status_line(ui, p.ok, p.ok, "Online"),
                                Some(_) => theme::status_line(ui, p.warn, p.warn, "Not faved you"),
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if target.is_some() {
                                if theme::button(ui, Kind::Secondary, None, "Cancel", true).clicked() {
                                    self.cancel.store(true, Ordering::Relaxed);
                                }
                            } else {
                                let can_send = !sending && !self.files.is_empty() && live.is_some_and(|peer| peer.mutual);
                                if theme::button(ui, Kind::Primary, Some(Icon::Send), "Send", can_send).clicked()
                                    && let Some(peer) = live
                                {
                                    actions.send_to = Some((peer.addr, fav.name.clone(), fav.id));
                                }
                                if can_browse
                                    && theme::icon_button(ui, Icon::Folder, "Browse files").clicked()
                                    && let Some(peer) = live
                                {
                                    actions.browse = Some((peer.addr, fav.name.clone()));
                                }
                            }
                        });
                    });
                });
            });
            if let Some(progress) = target {
                ui.add_space(4.0);
                let fraction = if progress.total_size == 0 { 1.0 } else { progress.total_sent as f32 / progress.total_size as f32 };
                theme::progress(ui, fraction);
                let rate = s
                    .send_started
                    .map(|t| t.elapsed().as_secs_f64().max(0.001))
                    .map_or(0, |secs| (progress.total_sent as f64 / secs) as u64);
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&progress.file_name).small().color(p.sub)).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        theme::sub(
                            ui,
                            &format!("{} of {}, {}/s", human_size(progress.total_sent), human_size(progress.total_size), human_size(rate)),
                        );
                    });
                });
            }
        });
    }

    // ---- received page ------------------------------------------------------------------------

    fn received_page(&mut self, ui: &mut egui::Ui, s: &Shared, actions: &mut Actions) {
        let p = pal(ui);

        for inc in &s.incoming {
            theme::card(ui, Some(p.accent), |ui| {
                let fraction = if inc.size == 0 { 1.0 } else { inc.received as f32 / inc.size as f32 };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    theme::tile(ui, Icon::Download, 36.0, Tint::Accent);
                    let mid_w = (ui.available_width() - 50.0 - 12.0).max(40.0);
                    ui.allocate_ui_with_layout(vec2(mid_w, 36.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(RichText::new(&inc.name).size(14.5)).truncate());
                        theme::sub(ui, &format!("Incoming from {}", inc.from));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{}%", (fraction * 100.0) as u32)).family(theme::bold()).color(p.accent_text));
                    });
                });
                ui.add_space(2.0);
                theme::progress(ui, fraction);
            });
        }

        for req in &s.requests {
            theme::banner(ui, p.warn_soft, |ui| {
                ui.label(RichText::new(format!("{} tried to send you files", req.name)).family(theme::bold()).color(p.warn));
                ui.label(RichText::new("Add it as a favorite to accept files from it.").size(13.0).color(p.warn));
                ui.horizontal(|ui| {
                    if theme::button(ui, Kind::Primary, None, "Add favorite", true).clicked() {
                        actions.add_favorite = Some((req.id, req.name.clone(), DeviceKind::Unknown));
                    }
                    if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                        actions.dismiss_request = Some(req.id);
                    }
                });
            });
        }

        if let Some(error) = self.open_error.clone() {
            theme::banner(ui, p.err_soft, |ui| {
                ui.label(RichText::new(error).color(p.err));
                if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                    self.open_error = None;
                }
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("History").family(theme::bold()).size(16.0));
            #[cfg(not(target_os = "android"))]
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::button(ui, Kind::Text, Some(Icon::Folder), "Open folder", true).clicked() {
                    let _ = std::fs::create_dir_all(&self.receive_dir);
                    platform::open_folder(&self.receive_dir);
                }
            });
        });

        if s.received.is_empty() {
            ui.label(RichText::new("Nothing received yet.").size(13.0).color(p.sub));
            return;
        }
        theme::card(ui, None, |ui| {
            for (i, item) in s.received.iter().rev().enumerate() {
                if i > 0 {
                    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().rect_filled(line, 0.0, p.line);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if item.ok {
                        theme::tile(ui, Icon::File, 36.0, Tint::Accent);
                    } else {
                        theme::tile(ui, Icon::Warning, 36.0, Tint::Error);
                    }
                    let open = item.ok && item.path.is_some();
                    let mid_w = (ui.available_width() - if open { 60.0 } else { 0.0 } - 12.0).max(40.0);
                    ui.allocate_ui_with_layout(vec2(mid_w, 36.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(RichText::new(&item.name).size(14.5)).truncate());
                        let ago = ago(item.at.elapsed());
                        let (detail, color) = if item.ok {
                            (format!("{} from {}, {ago}", human_size(item.size), item.from), p.sub)
                        } else {
                            (format!("{}, from {}, {ago}", item.note, item.from), p.err)
                        };
                        ui.add(egui::Label::new(RichText::new(detail).small().color(color)).truncate());
                    });
                    if open {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::button(ui, Kind::Text, None, "Open", true).clicked()
                                && let Some(path) = &item.path
                            {
                                self.open_error = platform::open_file(path).err();
                            }
                        });
                    }
                });
            }
        });
    }

    // ---- settings page ------------------------------------------------------------------------

    fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &Shared, actions: &mut Actions) {
        let p = pal(ui);

        theme::section(ui, "This device", None);
        theme::card(ui, None, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                theme::tile(ui, device_icon(platform::device_kind()), 44.0, Tint::Accent);
                let mid_w = (ui.available_width() - 80.0 - 12.0).max(40.0);
                ui.allocate_ui_with_layout(vec2(mid_w, 44.0), Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(egui::Label::new(RichText::new(&self.identity.name).family(theme::bold()).size(15.5)).truncate());
                    theme::sub(ui, &format!("Visible to nearby devices, id {}", self.identity.id.short()));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.name_edit.is_none() && theme::button(ui, Kind::Text, None, "Rename", true).clicked() {
                        self.name_edit = Some(self.identity.name.clone());
                    }
                });
            });
            let mut cancel = false;
            if let Some(text) = &mut self.name_edit {
                ui.add_space(8.0);
                ui.add(egui::TextEdit::singleline(text).hint_text("Device name").desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let ready = !text.trim().is_empty();
                    if theme::button(ui, Kind::Primary, None, "Save", ready).clicked() {
                        actions.rename = Some(text.clone());
                    }
                    if theme::button(ui, Kind::Text, None, "Use device name", true).clicked() {
                        actions.rename = Some(String::new());
                    }
                    if theme::button(ui, Kind::Text, None, "Cancel", true).clicked() {
                        cancel = true;
                    }
                });
            }
            if cancel {
                self.name_edit = None;
            }
        });

        theme::section(ui, "Receiving", None);
        theme::card(ui, None, |ui| {
            if self.receiver.is_some() {
                ui.label(RichText::new("Always on while the app is open").family(theme::bold()).size(14.5));
                ui.label(
                    RichText::new(format!(
                        "Files from favorites are accepted automatically on port {}.",
                        self.receiver.as_ref().map_or(DEFAULT_TCP_PORT, |r| r.port())
                    ))
                    .size(13.0)
                    .color(p.sub),
                );
            } else {
                if let Some(err) = &self.receiver_error {
                    ui.label(RichText::new(err).color(p.err));
                }
                if theme::button(ui, Kind::Primary, None, "Retry", true).clicked() {
                    self.restart_receiver(ctx);
                }
            }
            ui.add_space(4.0);
            theme::sub(ui, "Saved to");
            ui.label(RichText::new(self.receive_dir.display().to_string()).monospace());
            #[cfg(not(target_os = "android"))]
            ui.horizontal(|ui| {
                if theme::button(ui, Kind::Secondary, None, "Change…", true).clicked()
                    && let Some(dir) = platform::pick_folder()
                {
                    self.receive_dir = dir;
                    self.restart_receiver(ctx);
                }
                if theme::button(ui, Kind::Secondary, Some(Icon::Folder), "Open folder", true).clicked() {
                    let _ = std::fs::create_dir_all(&self.receive_dir);
                    platform::open_folder(&self.receive_dir);
                }
            });
            #[cfg(target_os = "android")]
            {
                ui.add_space(4.0);
                ui.label(RichText::new("File access").family(theme::bold()).size(14.5));
                if platform::has_all_files_access() {
                    ui.label(RichText::new("Allowed").size(13.0).family(theme::bold()).color(p.ok));
                } else {
                    ui.label(RichText::new("Received files are hidden in Android/data until you grant file access. Grant it to save into Download/AirLynx instead.").size(13.0).color(p.sub));
                    if theme::button(ui, Kind::Secondary, None, "Grant file access", true).clicked() {
                        platform::request_all_files_access();
                    }
                    if platform::is_tv() {
                        ui.label(RichText::new("Most TVs have no screen for this. From a computer: adb shell appops set dev.airlynx.app MANAGE_EXTERNAL_STORAGE allow").size(13.0).color(p.sub));
                    }
                }
            }
        });

        theme::section(ui, "Favorites", None);
        let favorites = self.favorites.list();
        if favorites.is_empty() {
            ui.label(RichText::new("No favorites yet.").size(13.0).color(p.sub));
        } else {
            theme::card(ui, None, |ui| {
                for (i, fav) in favorites.iter().enumerate() {
                    if i > 0 {
                        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                        ui.painter().rect_filled(line, 0.0, p.line);
                    }
                    let open = self.info_open == Some(fav.id);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        theme::tile(ui, device_icon(fav.kind), 36.0, Tint::Muted);
                        let mid_w = (ui.available_width() - 140.0 - 12.0).max(40.0);
                        ui.allocate_ui_with_layout(vec2(mid_w, 36.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add(egui::Label::new(RichText::new(&fav.name).size(14.5)).truncate());
                            theme::sub(ui, &fav.id.short());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::button(ui, Kind::Danger, None, "Remove", true).clicked() {
                                actions.remove_favorite = Some(fav.id);
                            }
                            if theme::button(ui, Kind::Text, None, if open { "Hide" } else { "Info" }, true).clicked() {
                                self.info_open = if open { None } else { Some(fav.id) };
                            }
                        });
                    });
                    if open {
                        self.favorite_details(ui, s, fav);
                    }
                }
            });
        }

        #[cfg(target_os = "android")]
        self.diagnostics_section(ui, ctx);

        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "AirLynx {}. Transfers are encrypted. Your device name is visible to everyone on the same network.",
                env!("CARGO_PKG_VERSION")
            ))
            .small()
            .color(p.sub),
        );
    }
}

impl AirLynxApp {
    /// What is known about one favorite: where it is, how we reach it and when it was seen.
    fn favorite_details(&self, ui: &mut egui::Ui, s: &Shared, fav: &airlynx_core::Favorite) {
        let p = pal(ui);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let sighting = self.favorites.sighting(fav.id);
        let mut rows = fav.details(sighting.as_ref(), now);
        let status = match s.online(fav.id) {
            None => "offline",
            Some(peer) if peer.mutual => "online",
            Some(_) => "online, not faved you",
        };
        rows.insert(1, ("Status", status.into()));
        ui.add_space(4.0);
        for (label, value) in rows {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.allocate_ui_with_layout(vec2(112.0, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_min_width(112.0);
                    ui.label(RichText::new(label).size(13.0).color(p.sub));
                });
                ui.add(egui::Label::new(RichText::new(value).size(13.0)).wrap());
            });
        }
        ui.add_space(4.0);
    }
}

#[cfg(target_os = "android")]
impl AirLynxApp {
    /// Battery and wakeup numbers for judging what listening costs. The screen being on skews them,
    /// so the log toggle exists to measure with the phone locked and read the result afterwards.
    fn diagnostics_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = pal(ui);
        theme::section(ui, "Diagnostics", None);
        self.diag.refresh();
        let mut experiments = self.diag.experiments();
        theme::card(ui, None, |ui| {
            theme::sub(
                ui,
                "Counted since the baseline. With the screen on the numbers are inflated: switch on the log, \
                 lock the phone for an hour or more, then send the log file to another device.",
            );
            ui.add_space(4.0);
            for (label, value) in self.diag.rows() {
                ui.scope(|ui| {
                    // Rows are as tall as a control by default, which leaves big gaps between lines of text.
                    ui.spacing_mut().interact_size.y = 0.0;
                    ui.spacing_mut().item_spacing = vec2(6.0, 2.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(label).size(13.0).color(p.sub));
                        ui.label(RichText::new(value).size(13.0));
                    });
                });
            }
            let exits = self.diag.exit_lines();
            if !exits.is_empty() {
                ui.add_space(4.0);
                theme::sub(ui, "How earlier runs of the app ended (newest first)");
                for line in exits {
                    ui.label(RichText::new(line).monospace().size(11.5));
                }
            }
            ui.add_space(4.0);
            let mut logging = self.diag.logging();
            if ui.checkbox(&mut logging, "Write a log file (every minute and on changes)").changed() {
                let path = self.receive_dir.join("airlynx-diag.log");
                self.diag.set_logging(logging, &path);
            }
            if let Some(err) = &self.diag.log_error {
                ui.label(RichText::new(err).size(13.0).color(p.err));
            }
            if let Some(path) = self.diag.log_path() {
                theme::sub(ui, "Log file, pick it with Browse files on the Send tab");
                ui.label(RichText::new(path.display().to_string()).monospace().size(12.0));
            }
            if theme::button(ui, Kind::Secondary, None, "Reset baseline", true).clicked() {
                self.diag.reset();
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Experiments").family(theme::bold()).size(14.5));
            theme::sub(
                ui,
                "Switch one thing off at a time and run it for 30 minutes or more with the screen off. \
                 Each change restarts the receiver and the baseline, and is recorded in the log.",
            );
            ui.checkbox(&mut experiments.receiver, "Receiver on (off is the control run)");
            ui.add_enabled_ui(experiments.receiver, |ui| {
                ui.checkbox(&mut experiments.wake_lock, "CPU wake lock during transfers");
                ui.checkbox(&mut experiments.wifi_lock, "Wi-Fi high-performance lock during transfers");
                ui.checkbox(&mut experiments.multicast_lock, "Multicast lock");
            });
        });
        if experiments != self.diag.experiments() {
            self.apply_experiments(ctx, experiments);
        }
    }

    /// Restarts the receiver with its locks as `experiments` says. The old locks have to go first,
    /// because `set_keep_awake` takes whatever `set_keep` last chose.
    fn apply_experiments(&mut self, ctx: &egui::Context, experiments: crate::diag::Experiments) {
        self.diag.set_experiments(experiments);
        self.receiver = None;
        platform::set_keep_awake(false);
        crate::android::set_keep(experiments.keep());
        self.start_receiver(ctx);
        // Stopping the old receiver should not count.
        self.diag.reset();
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..10 => "just now".into(),
        10..60 => format!("{}s ago", elapsed.as_secs()),
        60..3600 => format!("{} min ago", elapsed.as_secs() / 60),
        secs => format!("{} h ago", secs / 3600),
    }
}

fn device_icon(kind: DeviceKind) -> Icon {
    match kind {
        DeviceKind::Desktop => Icon::Computer,
        DeviceKind::Mobile => Icon::Phone,
        DeviceKind::Macsplorer => Icon::Macsplorer,
        DeviceKind::Unknown => Icon::Unknown,
    }
}

/// A small red count bubble.
fn badge(ui: &egui::Ui, center: egui::Pos2, count: usize) {
    let text = if count > 9 { "9+".to_string() } else { count.to_string() };
    let width = if count > 9 { 24.0 } else { 18.0 };
    let rect = egui::Rect::from_center_size(center, vec2(width, 18.0));
    ui.painter().rect_filled(rect, egui::CornerRadius::same(9), BADGE_COLOR);
    theme::text_at(ui, rect.center(), egui::Align2::CENTER_CENTER, &text, egui::FontId::new(11.5, theme::bold()), Color32::WHITE);
}

/// The outcome of the last send, shown above the file list until dismissed.
fn send_result(ui: &mut egui::Ui, s: &Shared, actions: &mut Actions) {
    let p = pal(ui);
    match &s.send {
        SendStatus::Idle | SendStatus::Sending(_) => {}
        SendStatus::Done(r) => {
            let secs = r.elapsed.as_secs_f64().max(0.001);
            let ok = r.failed.is_empty();
            theme::banner(ui, if ok { p.ok_soft } else { p.warn_soft }, |ui| {
                let color = if ok { p.ok } else { p.warn };
                ui.label(
                    RichText::new(format!("Sent {} file{} to {}", r.sent.len(), plural(r.sent.len()), s.send_to))
                        .family(theme::bold())
                        .color(color),
                );
                ui.label(
                    RichText::new(format!(
                        "{} in {secs:.1}s ({}/s)",
                        human_size(r.bytes),
                        human_size((r.bytes as f64 / secs) as u64)
                    ))
                    .size(13.0)
                    .color(color),
                );
                if !r.failed.is_empty() {
                    ui.label(RichText::new(format!("Failed verification: {}", r.failed.join(", "))).size(13.0).color(p.err));
                }
                if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                    actions.dismiss_send = true;
                }
            });
        }
        SendStatus::Failed(e) => {
            theme::banner(ui, p.err_soft, |ui| {
                ui.label(RichText::new(format!("Send to {} failed", s.send_to)).family(theme::bold()).color(p.err));
                ui.label(RichText::new(e).size(13.0).color(p.err));
                if theme::button(ui, Kind::Text, None, "Dismiss", true).clicked() {
                    actions.dismiss_send = true;
                }
            });
        }
    }
}

fn handle_receive_event(s: &mut Shared, event: ReceiveEvent) {
    match event {
        ReceiveEvent::Refused { id, name, .. } => {
            s.requests.retain(|r| r.id != id);
            s.requests.push(Request { id, name });
        }
        ReceiveEvent::NotWritable { peer, error } => {
            let from = s.name_for(&peer);
            s.received.push(Received {
                ok: false,
                name: "Transfer refused".into(),
                size: 0,
                from,
                note: format!("Save folder is not writable ({error})"),
                path: None,
                at: Instant::now(),
            });
        }
        // Looking into the save folder, or taking a file from it, shows nothing in History.
        ReceiveEvent::Connected { .. } | ReceiveEvent::Listed { .. } | ReceiveEvent::Served { .. } => {}
        ReceiveEvent::FileStarted { peer, name, size } => {
            s.incoming.retain(|i| i.peer != peer);
            let from = s.name_for(&peer);
            s.incoming.push(Incoming { peer, from, name, received: 0, size });
        }
        ReceiveEvent::FileProgress { peer, received, .. } => {
            if let Some(i) = s.incoming.iter_mut().find(|i| i.peer == peer) {
                i.received = received;
            }
        }
        ReceiveEvent::FileSaved { peer, path, size } => {
            platform::scan_file(&path);
            s.incoming.retain(|i| i.peer != peer);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let from = s.name_for(&peer);
            s.received.push(Received { ok: true, name, size, from, note: String::new(), path: Some(path), at: Instant::now() });
        }
        ReceiveEvent::FileRejected { peer, name, reason } => {
            s.incoming.retain(|i| i.peer != peer);
            let from = s.name_for(&peer);
            s.received.push(Received { ok: false, name, size: 0, from, note: format!("Rejected, {reason}"), path: None, at: Instant::now() });
        }
        ReceiveEvent::SessionEnded { peer, error } => {
            s.incoming.retain(|i| i.peer != peer);
            if let Some(e) = error {
                let from = s.name_for(&peer);
                s.received.push(Received { ok: false, name: "Transfer failed".into(), size: 0, from, note: e, path: None, at: Instant::now() });
            }
        }
    }
}

/// Android does not tell egui about the system dark mode, so ask the framework.
#[cfg(target_os = "android")]
fn sync_theme(ctx: &egui::Context) {
    if let Some(dark) = platform::is_dark_mode() {
        ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
        // Only on a change: the call goes through the UI thread.
        static LAST: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);
        if LAST.swap(dark as i8, Ordering::Relaxed) != dark as i8 {
            platform::set_system_bar_icons(dark);
        }
    }
}

impl eframe::App for AirLynxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        self.handle_keys(&ctx);

        #[cfg(not(target_os = "android"))]
        {
            let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
            if !dropped.is_empty() {
                self.add_files(dropped);
            }
        }

        let shared = self.shared.lock().unwrap().clone();
        if self.tab == Tab::Received {
            self.seen_received = shared.received.len();
        }
        let unseen = self.unseen(&shared);
        let wide = ctx.content_rect().width() >= WIDE_WIDTH;
        let browsing = self.browsing();
        let p = pal(ui);

        if wide {
            self.sidebar(ui, unseen);
        } else {
            self.header(ui, &ctx);
            if !browsing {
                self.tab_bar(ui, unseen);
            }
        }

        #[allow(unused_mut)]
        let mut margin = if wide { egui::Margin::symmetric(28, 24) } else { egui::Margin::symmetric(16, 4) };
        // The browser hides the tab bar, so it has to clear the gesture bar itself.
        #[cfg(target_os = "android")]
        if browsing {
            margin.bottom = (platform::navigation_bar_height_px() / ctx.pixels_per_point()) as i8;
        }
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(margin))
            .show(ui, |ui| {
                #[cfg(target_os = "android")]
                if let Some(browser) = &mut self.browser {
                    match browser.ui(ui) {
                        crate::browser::Action::None => {}
                        crate::browser::Action::Cancel => self.browser = None,
                        crate::browser::Action::Confirm(paths) => {
                            self.browser = None;
                            self.add_files(paths);
                        }
                    }
                    return;
                }

                if let Some(remote) = &mut self.remote {
                    if let remote::Action::Close = remote.ui(ui, &ctx) {
                        self.remote = None;
                    }
                    return;
                }

                if let Some(notice) = &self.notice {
                    theme::banner(ui, p.err_soft, |ui| {
                        ui.label(RichText::new(notice).color(p.err));
                    });
                }
                if wide {
                    ui.label(RichText::new(self.tab.title()).family(theme::bold()).size(24.0));
                    ui.add_space(4.0);
                }
                let mut actions = Actions::default();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match self.tab {
                        Tab::Send => self.send_page(ui, &ctx, &shared, wide, &mut actions),
                        Tab::Received => self.received_page(ui, &shared, &mut actions),
                        Tab::Settings => self.settings_page(ui, &ctx, &shared, &mut actions),
                    }
                    ui.add_space(12.0);
                });
                self.apply(&ctx, actions);
            });
    }
}

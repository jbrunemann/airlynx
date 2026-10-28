use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use airlynx_core::receive::{self, ReceiveEvent, ReceiverConfig, ReceiverHandle};
use airlynx_core::send::{self, Entry, Progress, Report};
use airlynx_core::{DEFAULT_TCP_PORT, DeviceId, Favorites, Identity, Peer, Profile, discover, human_size};
use eframe::egui::{self, Color32, RichText};

use crate::platform;

const OK_COLOR: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const ERR_COLOR: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);
const WARN_COLOR: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);

/// How often the network is scanned in the background.
const SCAN_INTERVAL: Duration = Duration::from_secs(6);
/// A device counts as online until it has been silent this long, so one lost
/// UDP packet does not make a favorite flicker offline.
const PEER_TTL: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    Transfer,
    History,
}

impl Tab {
    const ALL: [Tab; 2] = [Tab::Transfer, Tab::History];

    fn label(self) -> &'static str {
        match self {
            Tab::Transfer => "Transfer",
            Tab::History => "History",
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
    name: String,
    received: u64,
    size: u64,
}

#[derive(Clone)]
struct LogLine {
    ok: bool,
    text: String,
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
    /// Files the receiver confirmed; the UI drops them from the send list.
    delivered: Vec<PathBuf>,
    incoming: Vec<Incoming>,
    requests: Vec<Request>,
    log: Vec<LogLine>,
}

impl Shared {
    fn online(&self, id: DeviceId) -> Option<&Peer> {
        self.peers
            .iter()
            .find(|p| p.peer.id == id && p.seen.elapsed() < PEER_TTL)
            .map(|p| &p.peer)
    }
}

/// What the user asked for during one frame. Applied after drawing so the
/// widgets never borrow the app mutably.
#[derive(Default)]
struct Actions {
    send_to: Option<(SocketAddr, String)>,
    add_favorite: Option<(DeviceId, String)>,
    remove_favorite: Option<DeviceId>,
    dismiss_request: Option<DeviceId>,
    scan: bool,
}

pub struct AirLynxApp {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    identity: Identity,
    favorites: Favorites,
    notice: Option<String>,
    tab: Tab,
    files: Vec<Entry>,
    file_error: Option<String>,
    receive_dir: PathBuf,
    receiver: Option<ReceiverHandle>,
    receiver_error: Option<String>,
    last_scan: Instant,
    #[cfg(target_os = "android")]
    last_dir_check: Instant,
    #[cfg(target_os = "android")]
    browser: Option<crate::browser::FileBrowser>,
}

impl AirLynxApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            if cfg!(target_os = "android") {
                style.spacing.interact_size.y = 36.0;
            }
        });

        let (profile, notice) = match Profile::load(&platform::config_dir()) {
            Ok(profile) => (profile, None),
            // Still usable for this session, but favorites will be lost on exit.
            Err(e) => (
                Profile { id: DeviceId::random(), favorites: Favorites::in_memory() },
                Some(format!("Favorites cannot be saved: {e}")),
            ),
        };
        let device_name = platform::device_name();
        let mut app = Self {
            shared: Arc::default(),
            cancel: Arc::default(),
            identity: Identity::new(profile.id, &device_name),
            favorites: profile.favorites,
            notice,
            tab: Tab::default(),
            files: Vec::new(),
            file_error: None,
            receive_dir: platform::default_receive_dir(),
            receiver: None,
            receiver_error: None,
            last_scan: Instant::now(),
            #[cfg(target_os = "android")]
            last_dir_check: Instant::now(),
            #[cfg(target_os = "android")]
            browser: None,
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
            let result = discover(&identity, Duration::from_millis(1500));
            let mut s = shared.lock().unwrap();
            s.scanning = false;
            s.scan_visible = false;
            s.scanned_once = true;
            match result {
                Ok(found) => {
                    let now = Instant::now();
                    for peer in found {
                        // Keep stored favorite names current if the other device was renamed.
                        if favorites.contains(peer.id) {
                            let _ = favorites.add(peer.id, &peer.name);
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

    fn start_send(&mut self, ctx: &egui::Context, addr: SocketAddr, target_name: String) {
        let entries = self.files.clone();
        let shared = self.shared.clone();
        let cancel = self.cancel.clone();
        let identity = self.identity.clone();
        let ctx = ctx.clone();
        cancel.store(false, Ordering::Relaxed);
        {
            let mut s = shared.lock().unwrap();
            s.send_to = target_name;
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
        }
        let config = ReceiverConfig {
            dir: self.receive_dir.clone(),
            port: DEFAULT_TCP_PORT,
            identity: self.identity.clone(),
            favorites: self.favorites.clone(),
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
    /// once the user has granted file access.
    fn tick(&mut self, ctx: &egui::Context) {
        let delivered = std::mem::take(&mut self.shared.lock().unwrap().delivered);
        if !delivered.is_empty() {
            self.files.retain(|e| !delivered.contains(&e.path));
        }
        let scanning = self.shared.lock().unwrap().scanning;
        if !scanning && self.last_scan.elapsed() >= SCAN_INTERVAL {
            self.start_scan(ctx, false);
        }
        #[cfg(target_os = "android")]
        if self.last_dir_check.elapsed() >= Duration::from_secs(2) {
            self.last_dir_check = Instant::now();
            if self.receiver.is_some() && platform::default_receive_dir() != self.receive_dir {
                self.restart_receiver(ctx);
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// The Android file browser replaces the whole screen, so the tabs are hidden while it is open.
    fn browsing(&self) -> bool {
        #[cfg(target_os = "android")]
        return self.browser.is_some();
        #[cfg(not(target_os = "android"))]
        false
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Actions) {
        if let Some((addr, name)) = actions.send_to {
            self.start_send(ctx, addr, name);
        }
        if let Some((id, name)) = actions.add_favorite {
            self.set_notice(self.favorites.add(id, &name));
            self.shared.lock().unwrap().requests.retain(|r| r.id != id);
        }
        if let Some(id) = actions.remove_favorite {
            self.set_notice(self.favorites.remove(id));
        }
        if let Some(id) = actions.dismiss_request {
            self.shared.lock().unwrap().requests.retain(|r| r.id != id);
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

    fn set_notice(&mut self, result: std::io::Result<()>) {
        self.notice = result.err().map(|e| format!("Favorites cannot be saved: {e}"));
    }

    fn files_section(&mut self, ui: &mut egui::Ui, sending: bool) {
        section(ui, "Files to send");
        #[cfg(target_os = "android")]
        if platform::has_all_files_access() {
            if ui.add_enabled(!sending, egui::Button::new("Browse files…")).clicked() {
                self.browser = Some(crate::browser::FileBrowser::new(platform::storage_root()));
            }
        } else {
            ui.label("AirLynx needs access to your files to send them.");
            if ui.button("Grant file access").clicked() {
                platform::request_all_files_access();
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let add_clicked = ui
                .horizontal(|ui| {
                    let clicked = ui.add_enabled(!sending, egui::Button::new("Add files…")).clicked();
                    ui.label(RichText::new("or drop files onto this window").weak());
                    clicked
                })
                .inner;
            if add_clicked {
                self.add_files(platform::pick_files());
            }
        }

        if let Some(err) = &self.file_error {
            ui.colored_label(ERR_COLOR, err);
        }
        let mut remove = None;
        for (i, entry) in self.files.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.add_enabled(!sending, egui::Button::new("Remove").small()).clicked() {
                    remove = Some(i);
                }
                ui.label(&entry.name);
                ui.label(RichText::new(human_size(entry.size)).weak());
            });
        }
        if let Some(i) = remove {
            self.files.remove(i);
        }
        if self.files.len() > 1 && ui.add_enabled(!sending, egui::Button::new("Clear all")).clicked() {
            self.files.clear();
        }
    }

    fn favorites_section(&self, ui: &mut egui::Ui, s: &Shared, sending: bool, actions: &mut Actions) {
        section(ui, "Favorites");
        let favorites = self.favorites.list();
        if favorites.is_empty() {
            ui.label(
                RichText::new(
                    "No favorites yet. Add a device from Nearby below. Once both devices have added \
                     each other, they can send files to each other any time.",
                )
                .weak(),
            );
        } else if self.files.is_empty() {
            ui.label(RichText::new("Choose files above, then press Send next to a device.").weak());
        }
        for fav in &favorites {
            let live = s.online(fav.id);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&fav.name).strong());
                ui.label(RichText::new(fav.id.short()).weak().small());
            });
            ui.horizontal(|ui| {
                match live {
                    None => ui.label(RichText::new("Offline").weak()),
                    Some(p) if p.mutual => ui.colored_label(OK_COLOR, "Online"),
                    Some(_) => ui.colored_label(WARN_COLOR, "Online, has not added you yet"),
                };
                let can_send = !sending && !self.files.is_empty() && live.is_some_and(|p| p.mutual);
                if ui.add_enabled(can_send, egui::Button::new("Send")).clicked()
                    && let Some(p) = live
                {
                    actions.send_to = Some((p.addr, fav.name.clone()));
                }
                if ui.add(egui::Button::new("Unfavorite").small()).clicked() {
                    actions.remove_favorite = Some(fav.id);
                }
            });
        }
    }

    fn nearby_section(&self, ui: &mut egui::Ui, s: &Shared, actions: &mut Actions) {
        section(ui, "Nearby");
        ui.horizontal(|ui| {
            if ui.add_enabled(!s.scan_visible, egui::Button::new("Scan now")).clicked() {
                actions.scan = true;
            }
            if s.scan_visible {
                ui.spinner();
            }
        });
        if let Some(err) = &s.scan_error {
            ui.colored_label(ERR_COLOR, format!("Scan failed: {err}"));
        }
        let mut any = false;
        for seen in &s.peers {
            let peer = &seen.peer;
            if seen.seen.elapsed() >= PEER_TTL || self.favorites.contains(peer.id) {
                continue;
            }
            any = true;
            ui.horizontal(|ui| {
                ui.label(RichText::new(&peer.name).strong());
                ui.label(RichText::new(peer.id.short()).weak().small());
            });
            ui.horizontal(|ui| {
                if ui.button("Add favorite").clicked() {
                    actions.add_favorite = Some((peer.id, peer.name.clone()));
                }
                if peer.mutual {
                    ui.label(RichText::new("has added you").weak());
                }
            });
        }
        if !any && s.scanned_once && !s.scan_visible {
            ui.label(RichText::new("No new devices found. Is AirLynx open on the other device?").weak());
        }
    }

    fn send_status(&mut self, ui: &mut egui::Ui, s: &Shared) {
        match &s.send {
            SendStatus::Idle => {}
            SendStatus::Sending(p) => {
                section(ui, &format!("Sending to {}", s.send_to));
                let fraction = if p.total_size == 0 { 1.0 } else { p.total_sent as f32 / p.total_size as f32 };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!("{} ({}/{})", p.file_name, p.file_index + 1, p.file_count)),
                );
                if ui.button("Cancel").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                }
            }
            SendStatus::Done(r) => {
                let secs = r.elapsed.as_secs_f64().max(0.001);
                ui.colored_label(
                    OK_COLOR,
                    format!(
                        "Sent {} file(s) to {}, {} in {secs:.1}s ({}/s)",
                        r.sent.len(),
                        s.send_to,
                        human_size(r.bytes),
                        human_size((r.bytes as f64 / secs) as u64)
                    ),
                );
                if !r.failed.is_empty() {
                    ui.colored_label(ERR_COLOR, format!("Failed verification: {}", r.failed.join(", ")));
                }
            }
            SendStatus::Failed(e) => {
                ui.colored_label(ERR_COLOR, format!("Send to {} failed: {e}", s.send_to));
            }
        }
    }

    fn receive_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, s: &Shared, actions: &mut Actions) {
        section(ui, "Receiving");
        if let Some(handle) = &self.receiver {
            ui.colored_label(
                OK_COLOR,
                format!(
                    "Ready. Visible as \"{}\" on port {}. Files from favorites are accepted automatically.",
                    self.identity.name,
                    handle.port()
                ),
            );
        } else {
            if let Some(err) = &self.receiver_error {
                ui.colored_label(ERR_COLOR, err);
            }
            if ui.button("Retry").clicked() {
                self.restart_receiver(ctx);
            }
        }

        for req in &s.requests {
            ui.label(format!("{} tried to send you files, but is not a favorite.", req.name));
            ui.horizontal(|ui| {
                if ui.button("Add favorite").clicked() {
                    actions.add_favorite = Some((req.id, req.name.clone()));
                }
                if ui.button("Dismiss").clicked() {
                    actions.dismiss_request = Some(req.id);
                }
            });
        }

        ui.label(RichText::new("Saved to").weak());
        ui.label(RichText::new(self.receive_dir.display().to_string()).monospace());
        #[cfg(not(target_os = "android"))]
        ui.horizontal(|ui| {
            if ui.button("Change…").clicked()
                && let Some(dir) = platform::pick_folder()
            {
                self.receive_dir = dir;
                self.restart_receiver(ctx);
            }
            if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&self.receive_dir);
                platform::open_folder(&self.receive_dir);
            }
        });
        #[cfg(target_os = "android")]
        if !platform::has_all_files_access() {
            ui.label(RichText::new("Grant file access to save into Download/AirLynx instead.").weak());
            if ui.button("Grant file access").clicked() {
                platform::request_all_files_access();
            }
        }

        if !s.incoming.is_empty() {
            section(ui, "Incoming");
            for inc in &s.incoming {
                let fraction = if inc.size == 0 { 1.0 } else { inc.received as f32 / inc.size as f32 };
                ui.label(format!("{} from {}", inc.name, inc.peer));
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
            }
        }
    }

    fn history_tab(&self, ui: &mut egui::Ui, s: &Shared) {
        if s.log.is_empty() {
            ui.label(RichText::new("Nothing received yet.").weak());
        }
        for line in s.log.iter().rev() {
            let color = if line.ok { ui.visuals().text_color() } else { ERR_COLOR };
            ui.colored_label(color, &line.text);
        }
    }
}

fn handle_receive_event(s: &mut Shared, event: ReceiveEvent) {
    match event {
        ReceiveEvent::Refused { id, name, .. } => {
            s.requests.retain(|r| r.id != id);
            s.requests.push(Request { id, name });
        }
        ReceiveEvent::Connected { .. } => {}
        ReceiveEvent::FileStarted { peer, name, size } => {
            s.incoming.retain(|i| i.peer != peer);
            s.incoming.push(Incoming { peer, name, received: 0, size });
        }
        ReceiveEvent::FileProgress { peer, received, .. } => {
            if let Some(i) = s.incoming.iter_mut().find(|i| i.peer == peer) {
                i.received = received;
            }
        }
        ReceiveEvent::FileSaved { peer, path, size } => {
            s.incoming.retain(|i| i.peer != peer);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            s.log.push(LogLine { ok: true, text: format!("Saved {name} ({})", human_size(size)) });
        }
        ReceiveEvent::FileRejected { peer, name, reason } => {
            s.incoming.retain(|i| i.peer != peer);
            s.log.push(LogLine { ok: false, text: format!("Rejected {name}: {reason}") });
        }
        ReceiveEvent::SessionEnded { peer, error } => {
            s.incoming.retain(|i| i.peer != peer);
            if let Some(e) = error {
                s.log.push(LogLine { ok: false, text: format!("Transfer from {peer} failed: {e}") });
            }
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().size(16.0));
}

impl eframe::App for AirLynxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);

        #[cfg(not(target_os = "android"))]
        {
            let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
            if !dropped.is_empty() {
                self.add_files(dropped);
            }
        }

        egui::Panel::top("header").show(ui, |ui| {
            #[cfg(target_os = "android")]
            ui.add_space(platform::status_bar_height_px() / ctx.pixels_per_point());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("AirLynx").strong());
                ui.label(RichText::new(&self.identity.name).weak());
            });
            if let Some(notice) = &self.notice {
                ui.colored_label(ERR_COLOR, notice);
            }
            if !self.browsing() {
                ui.horizontal(|ui| {
                    for tab in Tab::ALL {
                        ui.selectable_value(&mut self.tab, tab, tab.label());
                    }
                });
            }
            ui.add_space(4.0);
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
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

            let shared = self.shared.lock().unwrap().clone();
            let sending = matches!(shared.send, SendStatus::Sending(_));
            let mut actions = Actions::default();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Transfer => {
                    self.files_section(ui, sending);
                    self.favorites_section(ui, &shared, sending, &mut actions);
                    self.send_status(ui, &shared);
                    self.nearby_section(ui, &shared, &mut actions);
                    self.receive_section(ui, &ctx, &shared, &mut actions);
                }
                Tab::History => self.history_tab(ui, &shared),
            });
            self.apply(&ctx, actions);
        });
    }
}

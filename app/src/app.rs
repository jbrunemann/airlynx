use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use airlynx_core::receive::{self, ReceiveEvent, ReceiverConfig, ReceiverHandle};
use airlynx_core::send::{self, Entry, Progress, Report};
use airlynx_core::{DEFAULT_TCP_PORT, Peer, discover, human_size};
use eframe::egui::{self, Color32, RichText};

use crate::platform;

const OK_COLOR: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const ERR_COLOR: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Send,
    Receive,
}

#[derive(Default)]
enum SendStatus {
    #[default]
    Idle,
    Sending(Progress),
    Done(Report),
    Failed(String),
}

/// An in-flight incoming file; one per connected sender.
struct Incoming {
    peer: String,
    name: String,
    received: u64,
    size: u64,
}

struct LogLine {
    ok: bool,
    text: String,
}

/// State written by background threads and read by the UI.
#[derive(Default)]
struct Shared {
    scanning: bool,
    scanned_once: bool,
    peers: Vec<Peer>,
    scan_error: Option<String>,
    send: SendStatus,
    incoming: Vec<Incoming>,
    log: Vec<LogLine>,
}

pub struct AirLynxApp {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    tab: Tab,
    device_name: String,
    files: Vec<Entry>,
    file_error: Option<String>,
    selected_peer: Option<SocketAddr>,
    manual_target: String,
    receive_dir: PathBuf,
    receiver: Option<ReceiverHandle>,
    receiver_error: Option<String>,
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

        let mut app = Self {
            shared: Arc::default(),
            cancel: Arc::default(),
            // Phones mostly send, computers mostly receive.
            tab: if cfg!(target_os = "android") { Tab::Send } else { Tab::Receive },
            device_name: platform::device_name(),
            files: Vec::new(),
            file_error: None,
            selected_peer: None,
            manual_target: String::new(),
            receive_dir: platform::default_receive_dir(),
            receiver: None,
            receiver_error: None,
            #[cfg(target_os = "android")]
            browser: None,
        };
        match app.tab {
            Tab::Send => app.start_scan(&cc.egui_ctx),
            Tab::Receive => app.start_receiver(&cc.egui_ctx),
        }
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

    fn start_scan(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        shared.lock().unwrap().scanning = true;
        thread::spawn(move || {
            let result = discover(Duration::from_millis(1500));
            let mut s = shared.lock().unwrap();
            s.scanning = false;
            s.scanned_once = true;
            match result {
                Ok(peers) => {
                    s.peers = peers;
                    s.scan_error = None;
                }
                Err(e) => s.scan_error = Some(e.to_string()),
            }
            ctx.request_repaint();
        });
    }

    fn target(&self) -> Option<Result<SocketAddr, String>> {
        if !self.manual_target.trim().is_empty() {
            return Some(send::resolve(&self.manual_target).map_err(|e| e.to_string()));
        }
        self.selected_peer.map(Ok)
    }

    fn start_send(&mut self, ctx: &egui::Context, addr: SocketAddr) {
        let entries = self.files.clone();
        let shared = self.shared.clone();
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        cancel.store(false, Ordering::Relaxed);
        shared.lock().unwrap().send = SendStatus::Sending(Progress {
            file_index: 0,
            file_count: entries.len(),
            file_name: String::new(),
            file_sent: 0,
            file_size: 0,
            total_sent: 0,
            total_size: entries.iter().map(|e| e.size).sum(),
        });

        thread::spawn(move || {
            let result = send::send_files(addr, &entries, |p| {
                shared.lock().unwrap().send = SendStatus::Sending(p.clone());
                ctx.request_repaint();
                if cancel.load(Ordering::Relaxed) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
            shared.lock().unwrap().send = match result {
                Ok(report) => SendStatus::Done(report),
                Err(e) => SendStatus::Failed(e.to_string()),
            };
            ctx.request_repaint();
        });
    }

    fn start_receiver(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "android")]
        {
            self.receive_dir = platform::default_receive_dir();
        }
        let config = ReceiverConfig {
            dir: self.receive_dir.clone(),
            port: DEFAULT_TCP_PORT,
            name: self.device_name.clone(),
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
                self.receiver_error = None;
            }
            Err(e) => self.receiver_error = Some(format!("Could not start receiver: {e}")),
        }
    }

    fn send_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let shared_arc = self.shared.clone();
        let shared = shared_arc.lock().unwrap();
        let sending = matches!(shared.send, SendStatus::Sending(_));

        section(ui, "Files");
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

        section(ui, "Receiver");
        let scan_clicked = ui
            .horizontal(|ui| {
                let clicked = ui.add_enabled(!shared.scanning, egui::Button::new("Scan network")).clicked();
                if shared.scanning {
                    ui.spinner();
                }
                clicked
            })
            .inner;
        if let Some(err) = &shared.scan_error {
            ui.colored_label(ERR_COLOR, format!("Scan failed: {err}"));
        }
        if shared.scanned_once && !shared.scanning && shared.peers.is_empty() {
            ui.label(RichText::new("No receivers found. Is AirLynx receiving on the other device?").weak());
        }
        for peer in &shared.peers {
            let selected = self.selected_peer == Some(peer.addr) && self.manual_target.trim().is_empty();
            let label = format!("{}   {}", peer.name, peer.addr.ip());
            if ui.selectable_label(selected, label).clicked() {
                self.selected_peer = Some(peer.addr);
                self.manual_target.clear();
            }
        }
        ui.horizontal(|ui| {
            ui.label("Or address:");
            ui.add(
                egui::TextEdit::singleline(&mut self.manual_target)
                    .hint_text("192.168.1.20")
                    .desired_width(180.0),
            );
        });

        ui.add_space(8.0);
        match &shared.send {
            SendStatus::Idle => {}
            SendStatus::Sending(p) => {
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
                        "Sent {} file(s), {} in {secs:.1}s ({}/s)",
                        r.sent.len(),
                        human_size(r.bytes),
                        human_size((r.bytes as f64 / secs) as u64)
                    ),
                );
                if !r.failed.is_empty() {
                    ui.colored_label(ERR_COLOR, format!("Failed verification: {}", r.failed.join(", ")));
                }
            }
            SendStatus::Failed(e) => {
                ui.colored_label(ERR_COLOR, format!("Send failed: {e}"));
            }
        }
        drop(shared);
        if scan_clicked {
            self.start_scan(&ctx);
        }

        let target = self.target();
        if let Some(Err(e)) = &target {
            ui.colored_label(ERR_COLOR, e);
        }
        let ready = !sending && !self.files.is_empty() && matches!(target, Some(Ok(_)));
        let button = egui::Button::new(RichText::new("Send").size(18.0)).min_size(egui::vec2(120.0, 36.0));
        if ui.add_enabled(ready, button).clicked()
            && let Some(Ok(addr)) = target
        {
            self.start_send(&ctx, addr);
        }
    }

    fn receive_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let running = self.receiver.is_some();

        section(ui, "Status");
        if let Some(handle) = &self.receiver {
            ui.colored_label(
                OK_COLOR,
                format!("Receiving. Visible as \"{}\" on port {}", self.device_name, handle.port()),
            );
            if ui.button("Stop receiving").clicked() {
                // Dropping the handle stops the listener.
                self.receiver = None;
            }
        } else {
            ui.label("Not receiving.");
            if ui.button("Start receiving").clicked() {
                self.start_receiver(&ctx);
            }
        }
        if let Some(err) = &self.receiver_error {
            ui.colored_label(ERR_COLOR, err);
        }

        section(ui, "Save to");
        ui.label(RichText::new(self.receive_dir.display().to_string()).monospace());
        #[cfg(not(target_os = "android"))]
        ui.horizontal(|ui| {
            if ui.add_enabled(!running, egui::Button::new("Change…")).clicked()
                && let Some(dir) = platform::pick_folder()
            {
                self.receive_dir = dir;
            }
            if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&self.receive_dir);
                platform::open_folder(&self.receive_dir);
            }
        });
        #[cfg(target_os = "android")]
        if !platform::has_all_files_access() {
            ui.label(RichText::new("Grant file access to save into Download/AirLynx instead.").weak());
            if ui.add_enabled(!running, egui::Button::new("Grant file access")).clicked() {
                platform::request_all_files_access();
            }
        }

        let shared = self.shared.lock().unwrap();
        if !shared.incoming.is_empty() {
            section(ui, "Incoming");
            for inc in &shared.incoming {
                let fraction = if inc.size == 0 { 1.0 } else { inc.received as f32 / inc.size as f32 };
                ui.label(format!("{} from {}", inc.name, inc.peer));
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
            }
        }
        if !shared.log.is_empty() {
            section(ui, "History");
            for line in shared.log.iter().rev() {
                let color = if line.ok { ui.visuals().text_color() } else { ERR_COLOR };
                ui.colored_label(color, &line.text);
            }
        }
    }
}

fn handle_receive_event(s: &mut Shared, event: ReceiveEvent) {
    match event {
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
        #[cfg(not(target_os = "android"))]
        {
            let dropped: Vec<PathBuf> = ui.ctx().input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
            if !dropped.is_empty() {
                self.add_files(dropped);
                self.tab = Tab::Send;
            }
        }

        egui::Panel::top("header").show(ui, |ui| {
            #[cfg(target_os = "android")]
            ui.add_space(platform::status_bar_height_px() / ui.ctx().pixels_per_point());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("AirLynx").strong());
                ui.label(RichText::new(&self.device_name).weak());
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Send, RichText::new("Send").size(16.0));
                ui.selectable_value(&mut self.tab, Tab::Receive, RichText::new("Receive").size(16.0));
            });
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
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Send => self.send_tab(ui),
                Tab::Receive => self.receive_tab(ui),
            });
        });
    }
}

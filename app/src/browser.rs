//! Minimal in-app file browser for Android, where no native picker is reachable
//! from a NativeActivity.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use airlynx_core::human_size;
use eframe::egui::{self, RichText};

use crate::theme::{self, Icon, Kind, pal};

pub enum Action {
    None,
    Cancel,
    Confirm(Vec<PathBuf>),
}

struct Item {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
}

pub struct FileBrowser {
    root: PathBuf,
    cwd: PathBuf,
    items: Vec<Item>,
    selected: BTreeSet<PathBuf>,
    error: Option<String>,
    /// Set when the directory changes so the next frame starts the list at the top.
    scroll_to_top: bool,
}

impl FileBrowser {
    pub fn new(root: PathBuf) -> Self {
        let mut browser = Self {
            cwd: root.clone(),
            root,
            items: Vec::new(),
            selected: BTreeSet::new(),
            error: None,
            scroll_to_top: false,
        };
        browser.refresh();
        browser
    }

    fn open(&mut self, dir: &Path) {
        self.cwd = dir.to_path_buf();
        self.scroll_to_top = true;
        self.refresh();
    }

    fn refresh(&mut self) {
        self.items.clear();
        self.error = None;
        let read = match std::fs::read_dir(&self.cwd) {
            Ok(read) => read,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            self.items.push(Item {
                name,
                path: entry.path(),
                is_dir: meta.is_dir(),
                size: meta.len(),
            });
        }
        self.items
            .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let mut action = Action::None;
        let mut navigate = None;

        ui.horizontal(|ui| {
            if theme::button(ui, Kind::Secondary, None, "Cancel", true).clicked() {
                action = Action::Cancel;
            }
            let label = format!("Add {} file(s)", self.selected.len());
            if theme::button(ui, Kind::Primary, Some(Icon::Plus), &label, !self.selected.is_empty()).clicked() {
                action = Action::Confirm(self.selected.iter().cloned().collect());
            }
        });
        ui.horizontal(|ui| {
            let at_root = self.cwd == self.root;
            if theme::button(ui, Kind::Secondary, None, "Up", !at_root).clicked() {
                navigate = self.cwd.parent().map(Path::to_path_buf);
            }
            let relative = self.cwd.strip_prefix(&self.root).unwrap_or(&self.cwd);
            ui.label(RichText::new(format!("/{}", relative.display())).monospace());
        });
        ui.separator();

        if let Some(err) = &self.error {
            ui.colored_label(pal(ui).err, err);
        }
        let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
        if std::mem::take(&mut self.scroll_to_top) {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            for item in &self.items {
                if item.is_dir {
                    if folder_row(ui, &item.name).clicked() {
                        navigate = Some(item.path.clone());
                    }
                } else {
                    let mut checked = self.selected.contains(&item.path);
                    let text = format!("{}  ({})", item.name, human_size(item.size));
                    if ui.checkbox(&mut checked, text).changed() {
                        if checked {
                            self.selected.insert(item.path.clone());
                        } else {
                            self.selected.remove(&item.path);
                        }
                    }
                }
            }
        });

        if let Some(dir) = navigate {
            self.open(&dir);
        }
        action
    }
}

/// A full-width tappable row for a folder.
fn folder_row(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let p = pal(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::click());
    let fill = if response.hovered() { p.accent_soft } else { p.surface };
    ui.painter().rect_filled(rect, egui::CornerRadius::same(12), fill);
    ui.painter().rect_stroke(rect, egui::CornerRadius::same(12), egui::Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
    Icon::Folder.paint(ui.painter(), egui::Rect::from_center_size(rect.left_center() + egui::vec2(24.0, 0.0), egui::Vec2::splat(22.0)), p.accent_text, 1.8);
    theme::text_at(ui, rect.left_center() + egui::vec2(48.0, 0.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(15.0), p.text);
    response
}

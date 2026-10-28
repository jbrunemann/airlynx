//! Minimal in-app file browser for Android, where no native picker is reachable
//! from a NativeActivity.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use airlynx_core::human_size;
use eframe::egui::{self, RichText};

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
}

impl FileBrowser {
    pub fn new(root: PathBuf) -> Self {
        let mut browser = Self {
            cwd: root.clone(),
            root,
            items: Vec::new(),
            selected: BTreeSet::new(),
            error: None,
        };
        browser.refresh();
        browser
    }

    fn open(&mut self, dir: &Path) {
        self.cwd = dir.to_path_buf();
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
            if ui.button("Cancel").clicked() {
                action = Action::Cancel;
            }
            let label = format!("Add {} file(s)", self.selected.len());
            if ui.add_enabled(!self.selected.is_empty(), egui::Button::new(label)).clicked() {
                action = Action::Confirm(self.selected.iter().cloned().collect());
            }
        });
        ui.horizontal(|ui| {
            let at_root = self.cwd == self.root;
            if ui.add_enabled(!at_root, egui::Button::new("Up")).clicked() {
                navigate = self.cwd.parent().map(Path::to_path_buf);
            }
            let relative = self.cwd.strip_prefix(&self.root).unwrap_or(&self.cwd);
            ui.label(RichText::new(format!("/{}", relative.display())).monospace());
        });
        ui.separator();

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(0xe5, 0x53, 0x4b), err);
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for item in &self.items {
                if item.is_dir {
                    if ui.button(format!("{}/", item.name)).clicked() {
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

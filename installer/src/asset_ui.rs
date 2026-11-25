use std::borrow::Cow;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{FontId, Label, RichText, Ui};

use crate::asset_folders::{self, AssetFolders};
use crate::{caption, field_label, secondary};

pub fn folder_option(ui: &mut Ui, label: &str, path: &mut Option<PathBuf>, fallback: &str) -> bool {
    folder_picker(ui, label, path, fallback, false)
}

pub(crate) fn compact_folder_option(
    ui: &mut Ui,
    label: &str,
    path: &mut Option<PathBuf>,
    fallback: &str,
) -> bool {
    folder_picker(ui, label, path, fallback, true)
}

fn folder_picker(
    ui: &mut Ui,
    label: &str,
    path: &mut Option<PathBuf>,
    fallback: &str,
    compact: bool,
) -> bool {
    let value = |ui: &mut Ui, path: &Option<PathBuf>| {
        let fallback_path = Path::new(fallback);
        if let Some(path) = path
            .as_deref()
            .or_else(|| fallback_path.is_absolute().then_some(fallback_path))
        {
            if compact {
                compact_path_label(ui, &readable_path(path));
            } else {
                caption(ui, &path.to_string_lossy());
            }
        } else {
            caption(ui, fallback);
        }
    };
    let actions = |ui: &mut Ui, path: &mut Option<PathBuf>| {
        let button = |ui: &mut Ui, title| {
            if compact {
                crate::monitor_button(ui, title, true, crate::SURFACE)
            } else {
                secondary(ui, title, true)
            }
        };
        let mut changed = false;
        if button(ui, "Browse…").clicked() {
            let mut dialog = rfd::FileDialog::new().set_title(label);
            if let Some(existing) = &path {
                dialog = dialog.set_directory(existing);
            }
            if let Some(directory) = dialog.pick_folder() {
                *path = Some(directory);
                changed = true;
            }
        }
        if path.is_some() && button(ui, "Clear").clicked() {
            *path = None;
            changed = true;
        }
        changed
    };
    if compact && ui.available_width() >= 300.0 {
        let action_width = if path.is_some() { 154.0 } else { 100.0 };
        let width = (ui.available_width() - action_width).max(92.0);
        return ui
            .horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    eframe::egui::vec2(width, 0.0),
                    eframe::egui::Layout::top_down(eframe::egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        field_label(ui, label);
                        value(ui, path);
                    },
                );
                actions(ui, path)
            })
            .inner;
    }
    field_label(ui, label);
    let changed = ui.horizontal_wrapped(|ui| actions(ui, path)).inner;
    value(ui, path);
    changed
}

pub(crate) fn readable_path(path: &Path) -> Cow<'_, str> {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        match prefix.kind() {
            std::path::Prefix::VerbatimDisk(_) => {
                if let Some(drive_path) = text.strip_prefix(r"\\?\") {
                    return Cow::Owned(drive_path.to_owned());
                }
            }
            std::path::Prefix::VerbatimUNC(..) => {
                if let Some(network_path) = text.strip_prefix(r"\\?\UNC\") {
                    return Cow::Owned(format!(r"\\{network_path}"));
                }
            }
            _ => {}
        }
    }
    text
}

fn compact_path_label(ui: &mut Ui, full_path: &str) {
    let color = crate::theme::color(ui.ctx(), crate::MUTED);
    let width = ui.available_width();
    let displayed = middle_elide(full_path, |value| {
        ui.painter()
            .layout_no_wrap(value.to_owned(), FontId::proportional(14.0), color)
            .size()
            .x
            <= width
    });
    ui.add(
        Label::new(RichText::new(displayed.as_ref()).size(14.0).color(color))
            .truncate()
            .selectable(false),
    )
    .on_hover_ui(|ui| {
        ui.add(Label::new(full_path).selectable(false));
    });
}

fn middle_elide(text: &str, fits: impl Fn(&str) -> bool) -> Cow<'_, str> {
    if fits(text) {
        return Cow::Borrowed(text);
    }
    let boundaries: Vec<_> = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect();
    let count = boundaries.len() - 1;
    let mut low = 0;
    let mut high = count;
    let mut best = "…".to_owned();
    while low < high {
        let keep = (low + high) / 2;
        let start = keep.div_ceil(3);
        let tail = keep - start;
        let candidate = format!(
            "{}…{}",
            &text[..boundaries[start]],
            &text[boundaries[count - tail]..]
        );
        if fits(&candidate) {
            best = candidate;
            low = keep + 1;
        } else {
            high = keep;
        }
    }
    Cow::Owned(best)
}

#[cfg(test)]
mod path_label_tests {
    use super::*;

    #[test]
    fn keeps_the_drive_and_destination_visible_in_an_elided_path() {
        let source = r"C:\Projects\部品\RenderBridge\Models";
        let display = middle_elide(source, |value| value.chars().count() <= 18);
        assert!(display.starts_with(r"C:\"));
        assert!(display.ends_with("Models"));
        assert!(display.contains('…'));
        assert!(display.chars().count() <= 18);
        assert_eq!(middle_elide(source, |_| true), source);
    }

    #[cfg(windows)]
    #[test]
    fn formats_extended_windows_paths_for_display_only() {
        let disk = Path::new(r"\\?\C:\Render Bridge\models");
        let unc = Path::new(r"\\?\UNC\machine\share\models");
        assert_eq!(readable_path(disk), r"C:\Render Bridge\models");
        assert_eq!(readable_path(unc), r"\\machine\share\models");
        assert_eq!(disk.as_os_str(), r"\\?\C:\Render Bridge\models");
    }
}

#[derive(Default)]
pub struct AssetEditor {
    pub folders: AssetFolders,
    pub message: Option<String>,
    #[cfg(windows)]
    copy: Option<Receiver<String>>,
}

impl AssetEditor {
    pub fn load(&mut self, data_dir: &Path) {
        match asset_folders::load(data_dir) {
            Ok(folders) => {
                self.folders = folders;
                self.message = None;
            }
            Err(error) => self.message = Some(error),
        }
    }

    pub fn fields(&mut self, ui: &mut Ui) {
        compact_folder_option(
            ui,
            "Generated assets folder",
            &mut self.folders.library,
            "Optional",
        );
        ui.separator();
        compact_folder_option(ui, "Backup folder", &mut self.folders.backup, "Optional");
        caption(
            ui,
            "Copy finished assets into these folders from Settings. ComfyUI keeps its original output.",
        );
    }

    #[cfg(windows)]
    pub fn settings(&mut self, ui: &mut Ui, output: &Path, input: &Path, data: &Path) {
        if let Some(copy) = &self.copy {
            match copy.try_recv() {
                Ok(message) => {
                    self.message = Some(message);
                    self.copy = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.message = Some("The asset copy did not finish.".into());
                    self.copy = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        ui.add_enabled_ui(self.copy.is_none(), |ui| {
            compact_folder_option(
                ui,
                "Generated assets folder",
                &mut self.folders.library,
                "Optional",
            );
            compact_folder_option(ui, "Backup folder", &mut self.folders.backup, "Optional");
            ui.horizontal_wrapped(|ui| {
                if crate::monitor_button(ui, "Save folders", true, crate::SURFACE).clicked() {
                    self.message = Some(
                        asset_folders::validate(output, input, data, &self.folders)
                            .and_then(|()| asset_folders::save(data, &self.folders))
                            .map_or_else(|error| error, |()| "Folder choices saved.".into()),
                    );
                }
                let copyable = self.folders.library.is_some() || self.folders.backup.is_some();
                if crate::monitor_button(ui, "Copy generated assets", copyable, crate::SURFACE)
                    .clicked()
                {
                    match asset_folders::validate(output, input, data, &self.folders)
                        .and_then(|()| asset_folders::save(data, &self.folders))
                    {
                        Ok(()) => self.start_copy(ui, output, input, data),
                        Err(error) => self.message = Some(error),
                    }
                }
            });
        });
        if let Some(message) = &self.message {
            caption(ui, message);
        }
    }

    #[cfg(windows)]
    fn start_copy(&mut self, ui: &Ui, output: &Path, input: &Path, data: &Path) {
        let (sender, receiver) = mpsc::channel();
        let (output, input, data) = (output.to_owned(), input.to_owned(), data.to_owned());
        let folders = self.folders.clone();
        let ctx = ui.ctx().clone();
        if std::thread::Builder::new()
            .name("copy-render-bridge-assets".into())
            .spawn(move || {
                let message = asset_folders::copy_generated(&output, &input, &data, &folders)
                    .map_or_else(|error| error, |summary| summary.message());
                let _ = sender.send(message);
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.message = Some("Copying generated assets…".into());
            self.copy = Some(receiver);
        } else {
            self.message = Some("Could not start the asset copy.".into());
        }
    }
}

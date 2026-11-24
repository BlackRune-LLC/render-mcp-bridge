use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::Ui;

use crate::asset_folders::{self, AssetFolders};
use crate::{caption, field_label, secondary};

pub fn folder_option(ui: &mut Ui, label: &str, path: &mut Option<PathBuf>, fallback: &str) -> bool {
    field_label(ui, label);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        let picker = secondary(ui, "Browse…", true);
        if picker.clicked() {
            let mut dialog = rfd::FileDialog::new().set_title(label);
            if let Some(existing) = &path {
                dialog = dialog.set_directory(existing);
            }
            if let Some(directory) = dialog.pick_folder() {
                *path = Some(directory);
                changed = true;
            }
        }
        if path.is_some() && secondary(ui, "Clear", true).clicked() {
            *path = None;
            changed = true;
        }
    });
    if let Some(path) = path {
        caption(ui, &path.to_string_lossy());
    } else {
        caption(ui, fallback);
    }
    changed
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
        folder_option(
            ui,
            "Generated assets folder",
            &mut self.folders.library,
            "Optional",
        );
        folder_option(ui, "Backup folder", &mut self.folders.backup, "Optional");
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
            self.fields(ui);
            ui.horizontal_wrapped(|ui| {
                if secondary(ui, "Save folders", true).clicked() {
                    self.message = Some(
                        asset_folders::validate(output, input, data, &self.folders)
                            .and_then(|()| asset_folders::save(data, &self.folders))
                            .map_or_else(|error| error, |()| "Folder choices saved.".into()),
                    );
                }
                let copyable = self.folders.library.is_some() || self.folders.backup.is_some();
                if secondary(ui, "Copy generated assets", copyable).clicked() {
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

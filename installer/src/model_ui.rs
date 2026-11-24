use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Ui};

use crate::model_library::{self, Library};
use crate::models::{DOWNLOAD_PIPELINES, ModelFile};
use crate::{
    CAUTION, MODEL_GUIDE, activity, asset_ui, caption, card, field_label, primary, secondary,
    status,
};

#[derive(PartialEq, Eq)]
struct Connection {
    url: String,
    default_root: PathBuf,
    data_dir: Option<PathBuf>,
}

struct Transfer {
    result: Receiver<Result<usize, String>>,
    cancel: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    name: &'static str,
}

#[derive(Default)]
pub struct ModelManager {
    connection: Option<Connection>,
    selected: Option<PathBuf>,
    destination_changed: bool,
    library: Option<Library>,
    scan: Option<Receiver<Result<Library, String>>>,
    transfer: Option<Transfer>,
    notice: Option<(String, bool)>,
    installed_only: bool,
    search: String,
}

impl ModelManager {
    pub fn downloading(&self) -> bool {
        self.transfer.is_some()
    }

    pub fn cancel(&self) {
        if let Some(transfer) = &self.transfer {
            transfer.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn destination_change(&self) -> Option<Option<&Path>> {
        self.destination_changed.then_some(self.selected.as_deref())
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let Some(connection) = &self.connection else {
            return;
        };
        let url = connection.url.clone();
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("inspect-model-library".into())
            .spawn(move || {
                let _ = sender.send(model_library::inspect(&url));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.scan = Some(result);
        } else {
            self.notice = Some(("Could not start the model check. Try again.".into(), false));
        }
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(scan) = &self.scan {
            match scan.try_recv() {
                Ok(Ok(library)) => {
                    self.scan = None;
                    self.library = Some(library);
                }
                Ok(Err(error)) => {
                    self.scan = None;
                    self.library = None;
                    self.notice = Some((error, false));
                }
                Err(TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.notice =
                        Some(("The model check did not finish. Try again.".into(), false));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let outcome = match self
            .transfer
            .as_ref()
            .map(|transfer| transfer.result.try_recv())
        {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("The model download stopped unexpectedly.".into())
            }
            _ => return,
        };
        self.transfer = None;
        self.notice = Some(match outcome {
            Ok(0) => (
                "The model files are already on disk. Checking the render backend.".into(),
                true,
            ),
            Ok(count) => (
                format!("Saved {count} model file(s). Checking the render backend."),
                true,
            ),
            Err(error) => (error, false),
        });
        self.refresh(ctx);
    }

    fn start_download(
        &mut self,
        name: &'static str,
        files: Vec<ModelFile>,
        root: PathBuf,
        ctx: &egui::Context,
    ) {
        let total = Arc::new(AtomicU64::new(files.iter().map(|file| file.size).sum()));
        let cancel = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let worker_cancel = Arc::clone(&cancel);
        let worker_bytes = Arc::clone(&bytes);
        let worker_total = Arc::clone(&total);
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("download-model-files".into())
            .spawn(move || {
                let outcome =
                    crate::model_download::install(&root, &files, &worker_cancel, |done, size| {
                        worker_total.store(size, Ordering::Relaxed);
                        worker_bytes.store(done, Ordering::Relaxed);
                    });
                let _ = sender.send(outcome);
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.transfer = Some(Transfer {
                result,
                cancel,
                bytes,
                total,
                name,
            });
            self.notice = None;
        } else {
            self.notice = Some((
                "Could not start the model download. Try again.".into(),
                false,
            ));
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        url: Option<&str>,
        default_root: PathBuf,
        data_dir: Option<&Path>,
    ) {
        let connection = url.map(|url| Connection {
            url: url.into(),
            default_root,
            data_dir: data_dir.map(Path::to_owned),
        });
        if self.connection != connection && !self.downloading() {
            self.selected = data_dir.and_then(model_library::load_destination);
            self.destination_changed = false;
            self.connection = connection;
            self.library = None;
            self.scan = None;
            self.notice = None;
            self.refresh(ui.ctx());
        }
        if self.connection.is_none() {
            caption(
                ui,
                "Choose a running ComfyUI build in Render Bridge Setup to manage its models.",
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.installed_only, false, "Download");
            ui.selectable_value(&mut self.installed_only, true, "Installed");
            if secondary(ui, "Refresh", self.scan.is_none() && !self.downloading()).clicked() {
                self.notice = None;
                self.refresh(ui.ctx());
            }
        });
        if let Some(transfer) = &self.transfer {
            let done = transfer.bytes.load(Ordering::Relaxed);
            let total = transfer.total.load(Ordering::Relaxed);
            let ratio = if total == 0 {
                0.0
            } else {
                (done as f64 / total as f64).clamp(0.0, 1.0) as f32
            };
            field_label(ui, transfer.name);
            ui.add(egui::ProgressBar::new(ratio).show_percentage());
            caption(ui, &format!("{} of {}", file_size(done), file_size(total)));
            let cancelling = transfer.cancel.load(Ordering::Relaxed);
            if secondary(
                ui,
                if cancelling {
                    "Cancelling…"
                } else {
                    "Cancel download"
                },
                !cancelling,
            )
            .clicked()
            {
                self.cancel();
            }
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        if let Some((message, ok)) = &self.notice {
            if *ok {
                caption(ui, message);
            } else {
                status(ui, "Model manager", message, CAUTION);
            }
        }
        if self.scan.is_some() {
            activity(ui, "Checking the active model library…");
        }
        if self.installed_only {
            self.show_inventory(ui);
        } else {
            self.show_catalog(ui);
        }
    }

    fn show_inventory(&mut self, ui: &mut Ui) {
        let Some(library) = &self.library else {
            return;
        };
        let label = field_label(ui, "Find installed models");
        ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(280.0))
            .labelled_by(label);
        let search = self.search.trim().to_lowercase();
        let mut count = 0;
        for (category, files) in &library.files {
            let matching: Vec<_> = files
                .iter()
                .filter(|name| name.to_lowercase().contains(&search))
                .collect();
            if !matching.is_empty() {
                count += matching.len();
                ui.collapsing(
                    format!("{} ({})", category.replace('_', " "), matching.len()),
                    |ui| {
                        for name in matching {
                            ui.add(egui::Label::new(egui::RichText::new(name).monospace()).wrap());
                        }
                    },
                );
            }
        }
        if count == 0 {
            caption(
                ui,
                if search.is_empty() {
                    "This ComfyUI build reported no model files."
                } else {
                    "No installed models match your search."
                },
            );
        }
    }

    fn show_catalog(&mut self, ui: &mut Ui) {
        let downloading = self.downloading();
        if let Some(connection) = &self.connection {
            ui.collapsing("Model storage and Hugging Face", |ui| {
                ui.add_enabled_ui(!downloading, |ui| {
                    let changed = asset_ui::folder_option(
                        ui,
                        "Model storage folder",
                        &mut self.selected,
                        "Automatic. Use folders registered by the active ComfyUI build.",
                    );
                    self.destination_changed |= changed;
                    if changed && let Some(data_dir) = &connection.data_dir
                        && let Err(error) = model_library::save_destination(data_dir, self.selected.as_deref())
                    {
                        self.notice = Some((error, false));
                    }
                    if let Some(folder) = &self.selected
                        && secondary(ui, "Copy model paths", true).clicked()
                    {
                        ui.ctx().copy_text(crate::model_paths(folder));
                    }
                });
                caption(ui, if std::env::var_os("HF_TOKEN").is_some_and(|value| !value.is_empty()) {
                    "Optional HF_TOKEN is available to this app."
                } else {
                    "Public downloads need no sign-in. For gated access, accept the model terms on Hugging Face and provide an optional read token through HF_TOKEN."
                });
                ui.hyperlink_to("Hugging Face token setup", "https://huggingface.co/docs/huggingface_hub/en/package_reference/environment_variables#hftoken");
            });
        }
        let mut selection = None;
        for pipeline in DOWNLOAD_PIPELINES {
            let library = self.library.as_ref();
            let missing: Vec<_> = pipeline
                .files
                .iter()
                .filter(|file| library.is_none_or(|library| !library.has(file)))
                .cloned()
                .collect();
            let missing_bytes: u64 = missing.iter().map(|file| file.size).sum();
            let destination = match (&self.connection, library) {
                (Some(connection), Some(library)) if !missing.is_empty() => {
                    if missing.iter().all(|file| library.reports_category(file)) {
                        library.destination(
                            self.selected.as_deref(),
                            &connection.default_root,
                            &missing,
                        )
                    } else {
                        Err("This build did not report all model categories for this pack. Refresh, or update ComfyUI if the category is unsupported.".into())
                    }
                }
                _ => Err("Start ComfyUI and refresh to choose a download location.".into()),
            };
            card(ui, |ui| {
                field_label(ui, pipeline.name);
                caption(ui, pipeline.description);
                caption(ui, pipeline.license);
                if missing_bytes == 0 {
                    caption(ui, "Installed in the active library.");
                } else if let Ok(root) = &destination {
                    caption(
                        ui,
                        &format!("{} to {}", file_size(missing_bytes), root.display()),
                    );
                    if library.is_some_and(|library| !library.paths_reported()) {
                        caption(
                            ui,
                            "Confirm this is the model folder used by the active ComfyUI build.",
                        );
                    }
                    if primary(
                        ui,
                        "Download missing files",
                        !downloading && self.scan.is_none(),
                    )
                    .clicked()
                    {
                        selection = Some((pipeline.name, missing, root.clone()));
                    }
                } else if let Err(error) = &destination {
                    caption(ui, error);
                }
            });
        }
        if let Some((name, files, root)) = selection {
            self.start_download(name, files, root, ui.ctx());
        }
        ui.hyperlink_to("Model and pipeline guide", MODEL_GUIDE);
    }
}

fn file_size(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

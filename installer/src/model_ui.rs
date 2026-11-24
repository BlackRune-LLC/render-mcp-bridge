use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Ui};

use crate::model_download::{self, InstallOutcome, Target, Verification};
use crate::model_library::{self, Library};
use crate::models::DOWNLOAD_PIPELINES;
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
    result: Receiver<Result<Work, String>>,
    cancel: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    name: &'static str,
    verifying: bool,
}

enum Work {
    Installed(InstallOutcome),
    Verified(Verification),
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
    verification: Option<(&'static str, Verification)>,
    confirm_repair: bool,
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
        let Some(transfer) = &self.transfer else {
            return;
        };
        let outcome = match transfer.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("The model operation stopped unexpectedly.".into())
            }
            _ => return,
        };
        let name = transfer.name;
        let verifying = transfer.verifying;
        self.transfer = None;
        match outcome {
            Ok(Work::Verified(report)) => self.verification = Some((name, report)),
            Ok(Work::Installed(result)) => {
                self.verification = None;
                let mut message = if result.files == 0 {
                    "The model files are already on disk. Checking the render backend.".into()
                } else {
                    format!(
                        "Saved {} model file(s). Checking the render backend.",
                        result.files
                    )
                };
                for backup in result.backups {
                    message.push_str(&format!("\nOriginal kept at {}", backup.display()));
                }
                self.notice = Some((message, true));
            }
            Err(error) => self.notice = Some((error, false)),
        }
        if !verifying {
            self.refresh(ctx);
        }
    }

    fn start_work(
        &mut self,
        name: &'static str,
        targets: Vec<Target>,
        verifying: bool,
        ctx: &egui::Context,
    ) {
        let repairing = targets.iter().any(Target::is_repair);
        let total = Arc::new(AtomicU64::new(targets.iter().map(Target::bytes).sum()));
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
                let progress = |done, size| {
                    worker_total.store(size, Ordering::Relaxed);
                    worker_bytes.store(done, Ordering::Relaxed);
                };
                let outcome = if verifying {
                    model_download::verify(&targets, &worker_cancel, progress).map(Work::Verified)
                } else {
                    model_download::install(&targets, &worker_cancel, progress).map(Work::Installed)
                };
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
                verifying,
            });
            self.notice = None;
            if !repairing {
                self.verification = None;
            }
            self.confirm_repair = false;
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
            self.verification = None;
            self.confirm_repair = false;
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
                self.verification = None;
                self.confirm_repair = false;
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
            field_label(
                ui,
                &format!(
                    "{} {}",
                    if transfer.verifying {
                        "Verifying"
                    } else {
                        "Downloading"
                    },
                    transfer.name
                ),
            );
            ui.add(egui::ProgressBar::new(ratio).show_percentage());
            caption(ui, &format!("{} of {}", file_size(done), file_size(total)));
            let cancelling = transfer.cancel.load(Ordering::Relaxed);
            if secondary(
                ui,
                if cancelling {
                    "Stopping…"
                } else if transfer.verifying {
                    "Stop verification"
                } else {
                    "Pause download"
                },
                !cancelling,
            )
            .clicked()
            {
                self.cancel();
            }
            if !transfer.verifying {
                caption(
                    ui,
                    "Select Download missing files or repair again to resume a stopped transfer.",
                );
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
                    if changed {
                        self.verification = None;
                        self.confirm_repair = false;
                    }
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
        let mut verify_pack = None;
        let idle = !downloading && self.scan.is_none();
        for pipeline in DOWNLOAD_PIPELINES {
            let library = self.library.as_ref();
            let missing: Vec<_> = pipeline
                .files
                .iter()
                .filter(|file| library.is_none_or(|library| !library.has(file)))
                .cloned()
                .collect();
            let missing_bytes: u64 = missing.iter().map(|file| file.size).sum();
            let targets = match (&self.connection, library) {
                (Some(connection), Some(library)) if !missing.is_empty() => {
                    if missing.iter().all(|file| library.reports_category(file)) {
                        missing
                            .iter()
                            .map(|file| {
                                let directory = library.destination(
                                    self.selected.as_deref(),
                                    &connection.default_root,
                                    file,
                                )?;
                                Ok(Target::new(file.clone(), directory.join(file.filename)))
                            })
                            .collect::<Result<Vec<_>, String>>()
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
                    caption(ui, "All files appear in the active library.");
                } else if let Ok(targets) = &targets {
                    let directories = targets
                        .iter()
                        .filter_map(|target| target.path.parent())
                        .collect::<BTreeSet<_>>();
                    if directories.len() == 1 {
                        if let Some(folder) = directories.first() {
                            caption(
                                ui,
                                &format!("{} to {}", file_size(missing_bytes), folder.display()),
                            );
                        }
                    } else {
                        caption(
                            ui,
                            &format!(
                                "{} across {} registered model folders:",
                                file_size(missing_bytes),
                                directories.len()
                            ),
                        );
                        for folder in directories {
                            caption(ui, &folder.display().to_string());
                        }
                    }
                    if library.is_some_and(|library| !library.paths_reported()) {
                        caption(
                            ui,
                            "Confirm this is the model folder used by the active ComfyUI build.",
                        );
                    }
                    if primary(ui, "Download missing files", idle).clicked() {
                        selection = Some((pipeline.name, targets.clone()));
                    }
                } else if let Err(error) = &targets {
                    caption(ui, error);
                }
                if library
                    .is_some_and(|library| pipeline.files.iter().any(|file| library.has(file)))
                    && secondary(ui, "Verify installed files", idle).clicked()
                {
                    verify_pack = Some(pipeline);
                }
                if let Some((name, report)) = &self.verification
                    && *name == pipeline.name
                {
                    caption(
                        ui,
                        &format!("{} installed file(s) matched the catalog.", report.verified),
                    );
                    if !report.repair.is_empty() {
                        caption(
                            ui,
                            &format!(
                                "{} file(s) differ from the pinned downloads. Manual variants can differ.",
                                report.repair.len()
                            ),
                        );
                        if self.confirm_repair {
                            caption(
                                ui,
                                "Repair downloads the catalog copies and keeps the originals as backups.",
                            );
                            for target in &report.repair {
                                caption(ui, &target.path.display().to_string());
                            }
                            let bytes = report.repair.iter().map(Target::bytes).sum();
                            ui.horizontal_wrapped(|ui| {
                                if primary(ui, &format!("Repair ({})", file_size(bytes)), idle)
                                    .clicked()
                                {
                                    selection = Some((pipeline.name, report.repair.clone()));
                                }
                                if secondary(ui, "Cancel", idle).clicked() {
                                    self.confirm_repair = false;
                                }
                            });
                        } else if secondary(ui, "Review repair", idle).clicked() {
                            self.confirm_repair = true;
                        }
                    }
                }
            });
        }
        if let Some((name, targets)) = selection {
            self.start_work(name, targets, false, ui.ctx());
        } else if let Some(pipeline) = verify_pack
            && let (Some(connection), Some(library)) = (&self.connection, &self.library)
        {
            let targets = pipeline
                .files
                .iter()
                .filter(|file| library.has(file))
                .map(|file| {
                    library
                        .installed_path(self.selected.as_deref(), &connection.default_root, file)?
                        .map(|path| Target::new(file.clone(), path))
                        .ok_or(
                            "Refresh the active model library and try verifying again.".to_owned(),
                        )
                })
                .collect::<Result<Vec<_>, String>>();
            match targets {
                Ok(targets) => self.start_work(pipeline.name, targets, true, ui.ctx()),
                Err(error) => self.notice = Some((error, false)),
            }
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::{process, thread};

use eframe::egui;

use crate::setup::{self, Installed};
use crate::theme;
use crate::update_handoff::{self, UpdateRequest};
use crate::updates::{self, Release, StagedInstaller};
use crate::utility_ui::{self, UtilityIcon};
use crate::{
    CAUTION, CREAM, RAIL, activity, bridge_mark, caption, panel_modal, primary, secondary, status,
};

enum UpdateEvent {
    Checked(Result<Option<Release>, String>),
    Downloaded(Result<(Release, StagedInstaller), String>),
}

#[derive(Default)]
pub struct UpdateDialog {
    open: bool,
    downloading: bool,
    result: Option<Result<Option<Release>, String>>,
    worker: Option<Receiver<UpdateEvent>>,
}

impl UpdateDialog {
    pub fn check(&mut self, ctx: &egui::Context) {
        if self.open {
            return;
        }
        self.open = true;
        self.start_check(ctx);
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        self.result = None;
        self.run(ctx, || UpdateEvent::Checked(updates::check()));
    }

    fn run(&mut self, ctx: &egui::Context, action: impl FnOnce() -> UpdateEvent + Send + 'static) {
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("render-bridge-update".into())
            .spawn(move || {
                let _ = send.send(action());
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(receive),
            Err(_) => self.fail("The update worker could not start.".into()),
        }
    }

    fn fail(&mut self, message: String) {
        self.downloading = false;
        self.result = Some(Err(message));
    }

    pub fn blocks_controls(&self) -> bool {
        self.downloading
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        installed: Option<&Installed>,
        blocked: Option<&str>,
    ) -> bool {
        if !self.open {
            return false;
        }
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(UpdateEvent::Checked(result)) => {
                    self.worker = None;
                    self.result = Some(result);
                }
                Ok(UpdateEvent::Downloaded(result)) => {
                    self.worker = None;
                    self.downloading = false;
                    match result {
                        Ok((release, staging)) => {
                            let launched = installed
                                .filter(|installed| {
                                    blocked.is_none()
                                        && update_handoff::is_installed_monitor(&installed.data_dir)
                                })
                                .ok_or_else(|| {
                                    blocked
                                        .unwrap_or("Open the installed panel before updating.")
                                        .to_owned()
                                })
                                .and_then(|installed| {
                                    start_installer(&installed.data_dir, &release, staging)
                                });
                            match launched {
                                Ok(()) => return true,
                                Err(error) => self.fail(error),
                            }
                        }
                        Err(error) => self.fail(error),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.fail("The update check did not finish. Try again.".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let response = panel_modal(ctx, "render-bridge-update").show(ctx, |ui| {
            let viewport = ctx.content_rect();
            ui.set_width((viewport.width() - 48.0).clamp(120.0, 394.0));
            let detail = if self.downloading {
                "Verifying the installer".to_owned()
            } else {
                match &self.result {
                    Some(Ok(Some(release))) if release.is_installed_version() => {
                        "Published build available".to_owned()
                    }
                    Some(Ok(Some(release))) => format!("Version {} is available", release.version),
                    Some(Ok(None)) => "Render Bridge is up to date".to_owned(),
                    Some(Err(_)) => "Could not check for updates".to_owned(),
                    None => "Check for the latest release".to_owned(),
                }
            };
            utility_ui::heading(ui, UtilityIcon::Download, "Render Bridge Update", Some(&detail));
            utility_ui::scroll_body(ui, "update-dialog-body", |ui| {
                    caption(ui, concat!("Installed version ", env!("CARGO_PKG_VERSION")));
                    if self.downloading {
                        activity(ui, "Downloading and verifying the installer…");
                    } else {
                        match &self.result {
                            None => activity(ui, "Checking for updates…"),
                            Some(Ok(None)) => {}
                            Some(Ok(Some(_))) => {
                                caption(ui, "For an unsigned build, download the installer from GitHub. Finish any jobs and exit Render Bridge from the tray. Choose the same folder when you run setup.");
                                if let Some(reason) = blocked {
                                    caption(ui, reason);
                                } else {
                                    caption(
                                        ui,
                                        "A signed build can also be installed here.",
                                    );
                                }
                            }
                            Some(Err(error)) => {
                                status(ui, "Update unavailable", error, CAUTION);
                                ui.hyperlink_to("Open GitHub downloads", updates::RELEASES_URL);
                            }
                        }
                    }
            });
            utility_ui::footer(ui);
            let mut install = false;
            let mut retry = false;
            let mut close = false;
            ui.horizontal_wrapped(|ui| {
                if !self.downloading
                    && let Some(Ok(Some(release))) = &self.result
                {
                    if primary(ui, "Open GitHub download", true).clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(&release.url));
                    }
                    install = secondary(ui, "Install signed build", blocked.is_none()).clicked();
                } else if matches!(self.result, Some(Err(_))) {
                    retry = primary(ui, "Check again", true).clicked();
                }
                close = secondary(ui, if self.downloading { "Cancel" } else { "Close" }, true)
                    .clicked();
            });
            (install, retry, close)
        });
        let (install, retry, close) = response.inner;
        if close || (!self.downloading && response.should_close()) {
            self.open = false;
            self.worker = None;
            self.downloading = false;
        } else if retry {
            self.start_check(ctx);
        } else if install
            && let Some(Ok(Some(release))) = self.result.take()
            && let Some(installed) = installed
        {
            let url = setup::installed_comfy_url(installed);
            self.downloading = true;
            self.run(ctx, move || {
                let result = (|| {
                    ensure_queue_idle(url.as_deref())?;
                    let staged = updates::download(&release)?;
                    ensure_queue_idle(url.as_deref())?;
                    Ok((release, staged))
                })();
                UpdateEvent::Downloaded(result)
            });
        }
        false
    }
}

fn ensure_queue_idle(url: Option<&str>) -> Result<(), String> {
    if url
        .and_then(|url| crate::jobs::check(url).ok())
        .is_some_and(|queue| !queue.is_idle())
    {
        return Err(
            "Wait for running and queued jobs to finish before installing the update.".into(),
        );
    }
    Ok(())
}

fn start_installer(data: &Path, release: &Release, staging: StagedInstaller) -> Result<(), String> {
    setup::quiet_command(&staging.path)
        .arg("--apply-update")
        .arg(data)
        .arg(process::id().to_string())
        .arg(&release.version)
        .spawn()
        .map_err(|_| "Windows could not open the verified installer.".to_owned())?;
    let _ = staging.directory.keep();
    Ok(())
}

enum ApplyEvent {
    Progress(String),
    Done(Result<(), String>),
}

struct ApplyWindow {
    worker: Option<Receiver<ApplyEvent>>,
    target: Option<(PathBuf, PathBuf)>,
    progress: String,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl ApplyWindow {
    fn new(ctx: &egui::Context, request: Result<UpdateRequest, String>) -> Self {
        let data_dir = request
            .as_ref()
            .ok()
            .map(|request| request.data_dir.as_path());
        theme::configure_for(ctx, theme::load(data_dir), data_dir);
        let mut app = Self {
            worker: None,
            target: None,
            progress: "Waiting for Render Bridge to close…".into(),
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                app.error = Some(error);
                return app;
            }
        };
        app.target = Some((request.data_dir.clone(), request.cleanup_stage.clone()));
        let cancel = Arc::clone(&app.cancel);
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("apply-render-bridge-update".into())
            .spawn(move || {
                let result = request.wait_for_previous().and_then(|()| {
                    setup::upgrade_existing(
                        &request.data_dir,
                        &cancel,
                        |message| {
                            let _ = send.send(ApplyEvent::Progress(message.into()));
                            ctx.request_repaint();
                        },
                        update_handoff::replace_monitor,
                    )
                    .map(|_| ())
                });
                let _ = send.send(ApplyEvent::Done(result));
                ctx.request_repaint();
            }) {
            Ok(_) => app.worker = Some(receive),
            Err(_) => {
                app.error =
                    Some("The installer could not start. Open Render Bridge and try again.".into())
            }
        }
        app
    }

    fn reopen(&mut self, ctx: &egui::Context) {
        let Some((data, stage)) = &self.target else {
            return;
        };
        match update_handoff::launch_installed_monitor(data, stage) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(error) => self.error = Some(error),
        }
    }
}

impl eframe::App for ApplyWindow {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        while let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(ApplyEvent::Progress(progress)) => self.progress = progress,
                Ok(ApplyEvent::Done(result)) => {
                    self.worker = None;
                    match result {
                        Ok(()) => self.reopen(ctx),
                        Err(error) => self.error = Some(error),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.error = Some("The installer closed before completing the update.".into());
                }
            }
        }
        if self.worker.is_some() && ctx.input(|input| input.viewport().close_requested()) {
            self.cancel.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui::TopBottomPanel::top("update-banner")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::color(ctx, RAIL))
                    .inner_margin(egui::Margin::symmetric(16, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    bridge_mark(ui);
                    ui.label(
                        egui::RichText::new("Render Bridge")
                            .size(20.0)
                            .strong()
                            .color(theme::on_fill(theme::color(ctx, RAIL))),
                    );
                });
            });
        let cancelling = self.cancel.load(Ordering::Relaxed);
        egui::TopBottomPanel::bottom("update-apply-actions")
            .frame(
                egui::Frame::new()
                    .fill(theme::color(ctx, crate::SURFACE))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.error.is_some() {
                        if self.target.is_some() {
                            if secondary(ui, "Open Render Bridge", true).clicked() {
                                self.reopen(ctx);
                            }
                        } else if secondary(ui, "Close", true).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    } else if secondary(ui, "Cancel", !cancelling).clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    utility_ui::heading(ui, UtilityIcon::Download, "Updating Render Bridge", None);
                    if let Some(error) = &self.error {
                        status(ui, "Update could not finish", error, CAUTION);
                    } else {
                        crate::card(ui, |ui| {
                            activity(
                                ui,
                                if cancelling {
                                    "Cancelling the update…"
                                } else {
                                    &self.progress
                                },
                            );
                        });
                    }
                });
            });
    }
}

pub fn apply(request: Result<UpdateRequest, String>) -> eframe::Result {
    eframe::run_native(
        "Render Bridge Update",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_icon(crate::icons::window_icon())
                .with_inner_size(egui::vec2(500.0, 270.0))
                .with_min_inner_size(egui::vec2(340.0, 210.0)),
            ..Default::default()
        },
        Box::new(move |creation| Ok(Box::new(ApplyWindow::new(&creation.egui_ctx, request)))),
    )
}

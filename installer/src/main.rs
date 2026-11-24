#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod asset_folders;
mod asset_ui;
mod connection;
mod diagnostics;
mod discovery;
mod icons;
#[cfg(windows)]
mod job_progress;
mod jobs;
mod jobs_panel;
mod models;
#[cfg(windows)]
mod python_install;
mod setup;
mod shortcuts;
#[cfg(windows)]
mod startup;
mod theme;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod update_handoff;
#[cfg(windows)]
mod update_ui;
#[cfg(windows)]
mod updates;

use connection::{Client, RunningBridge};
use eframe::egui::{self, Color32, Frame, RichText, Stroke, Ui, Vec2};
use jobs_panel::QueueView;
use setup::{Installed, ModelCheck, PythonReadiness, Settings};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const RAIL: Color32 = Color32::from_rgb(48, 36, 80);
const RAIL_ACTIVE: Color32 = Color32::from_rgb(224, 212, 249);
const RAIL_TEXT: Color32 = Color32::from_rgb(232, 223, 248);
const INK: Color32 = Color32::from_rgb(32, 22, 50);
const CREAM: Color32 = Color32::from_rgb(255, 248, 236);
const SURFACE: Color32 = Color32::from_rgb(255, 253, 248);
const LINE: Color32 = Color32::from_rgb(222, 211, 233);
const MUTED: Color32 = Color32::from_rgb(91, 79, 105);
const VIOLET: Color32 = Color32::from_rgb(83, 53, 133);
const GREEN: Color32 = Color32::from_rgb(39, 99, 61);
const LIME: Color32 = Color32::from_rgb(201, 245, 109);
const MARK_SHADE: Color32 = Color32::from_rgb(180, 151, 237);
const CAUTION: Color32 = Color32::from_rgb(118, 72, 25);
const ERROR: Color32 = Color32::from_rgb(133, 46, 40);

const COMFY_GUIDE: &str = "https://docs.comfy.org/installation/manual_install";
const COMFY_DOWNLOAD: &str = "https://comfy.org/download";
const CLAUDE_SETUP: &str = "https://code.claude.com/docs/en/quickstart";
const CODEX_SETUP: &str = "https://openai.com/codex/get-started/";
const MODEL_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/docs/pipelines.md";
const CLIENT_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge#connect-an-mcp-client";
const PRIVACY_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/PRIVACY.md";
const REMOTE_GUIDE: &str = "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/docs/connections.md#connect-from-another-device";
#[cfg(windows)]
const SETTINGS_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/docs/configuration.md";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Prerequisites,
    Comfy,
    Models,
    Install,
    Connect,
}

const STEPS: [(Step, &str, &str); 5] = [
    (Step::Prerequisites, "01", "Prerequisites"),
    (Step::Comfy, "02", "ComfyUI"),
    (Step::Models, "03", "Models"),
    (Step::Install, "04", "Install"),
    (Step::Connect, "05", "Connect"),
];

enum Event {
    PythonChecked(PythonReadiness),
    #[cfg(windows)]
    PythonInstallFinished(Result<PythonReadiness, String>),
    Checked(CheckOutcome),
    Discovered(Vec<discovery::Instance>),
    Verified(Option<RunningBridge>, Result<serde_json::Value, String>),
    GamingModeChanged(Result<(), String>),
    TokenRotated(Result<(), String>),
    Progress(String),
    Finished(Result<Installed, String>),
}

enum CheckOutcome {
    Models(Vec<ModelCheck>, bool, Option<models::Hardware>),
    Offline(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueAction {
    Inspect,
    StopBridge,
    CloseWindow,
}

struct QueueDialog {
    queue: QueueView,
    action: QueueAction,
}

enum MonitorTarget {
    Installation(PathBuf),
    ComfyUrl(String),
    Missing,
}

enum LaunchMode {
    Setup,
    Controls {
        target: MonitorTarget,
        start_hidden: bool,
    },
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum ServerAction {
    Start,
    Stop,
    Exit,
}

#[cfg(windows)]
struct TrayPanel {
    icon: tray::Tray,
    hidden: bool,
    hidden_start_frames: u8,
    exiting: bool,
    server_action: Option<ServerAction>,
}

#[cfg(windows)]
struct StartupSetting {
    executable: PathBuf,
    arguments: Vec<std::ffi::OsString>,
    open: bool,
    enabled: Option<bool>,
    error: Option<String>,
    appearance_error: Option<String>,
    assets: asset_ui::AssetEditor,
}

#[cfg(windows)]
impl StartupSetting {
    fn open(&mut self, installed: Option<&Installed>) {
        self.open = true;
        if let Some(installed) = installed {
            self.assets.load(&installed.data_dir);
        }
        match startup::enabled(&self.executable, &self.arguments) {
            Ok(enabled) => {
                self.enabled = Some(enabled);
                self.error = None;
            }
            Err(message) => {
                self.enabled = None;
                self.error = Some(message);
            }
        }
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        appearance: &mut egui::ThemePreference,
        data_dir: Option<&Path>,
        installed: Option<&Installed>,
    ) {
        if !self.open {
            return;
        }
        let response = panel_modal(ctx, "render-bridge-settings").show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Render Bridge Settings");
            ui.horizontal_wrapped(|ui| {
                ui.label("Theme");
                for (label, preference) in [
                    ("Dark", egui::ThemePreference::Dark),
                    ("Light", egui::ThemePreference::Light),
                    ("System", egui::ThemePreference::System),
                ] {
                    let button = egui::Button::new(label)
                        .selected(*appearance == preference)
                        .min_size(egui::vec2(65.0, 30.0));
                    if ui.add(button).clicked() && *appearance != preference {
                        match theme::save(data_dir, preference) {
                            Ok(()) => {
                                *appearance = preference;
                                self.appearance_error = None;
                                ctx.set_theme(preference);
                                ctx.request_repaint();
                            }
                            Err(error) => self.appearance_error = Some(error),
                        }
                    }
                }
            });
            if let Some(error) = &self.appearance_error {
                status(ui, "Theme setting unavailable", error, CAUTION);
            }
            ui.separator();
            if let Some(mut enabled) = self.enabled
                && ui
                    .checkbox(&mut enabled, "Start hidden when I sign in")
                    .changed()
            {
                match startup::set_enabled(&self.executable, &self.arguments, enabled) {
                    Ok(()) => {
                        self.enabled = Some(enabled);
                        self.error = None;
                    }
                    Err(message) => self.error = Some(message),
                }
            }
            caption(
                ui,
                "Open Render Bridge in the Windows tray when you sign in.",
            );
            if let Some(error) = &self.error {
                status(ui, "Settings unavailable", error, CAUTION);
            }
            if let Some(installed) = installed {
                ui.separator();
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    ui.heading("Storage");
                    caption(ui, &format!("Installation folder: {}", installed.data_dir.display()));
                    match setup::saved_install_settings(installed) {
                        Ok(settings) => self.assets.settings(
                            ui, &setup::folder(&settings, "output"), &setup::folder(&settings, "input"),
                            &installed.data_dir,
                        ),
                        Err(error) => caption(ui, &error),
                    }
                    ui.separator();
                    ui.heading("Advanced configuration");
                    caption(ui, "Stop Render Bridge before editing its active configuration. Start it again to verify your changes.");
                    if secondary(ui, "Open configuration file", true).clicked() {
                        let file = setup::config_file(installed);
                        if setup::quiet_command("notepad.exe").arg(file).spawn().is_err() {
                            self.error = Some("Could not open the configuration file.".into());
                        }
                    }
                    ui.hyperlink_to("Server and model settings", SETTINGS_GUIDE);
                });
            }
            ui.horizontal_wrapped(|ui| {
                let logs = diagnostics::existing_directory();
                if secondary(ui, "Open crash logs", logs.is_some())
                    .on_disabled_hover_text("No local crash logs yet.")
                    .clicked()
                    && let Some(folder) = logs
                    && !diagnostics::open_directory(&folder)
                {
                    self.error = Some("Could not open the crash logs folder.".into());
                }
                secondary(ui, "Done", true).clicked()
            }).inner
        });
        if response.inner || response.should_close() {
            self.open = false;
        }
    }
}

#[cfg(windows)]
impl TrayPanel {
    fn hide(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn update(&mut self, ctx: &egui::Context, guard_exit: bool) -> bool {
        while let Some(event) = self.icon.try_recv() {
            match event {
                event @ (tray::TrayEvent::Open
                | tray::TrayEvent::StartServer
                | tray::TrayEvent::StopServer)
                    if !self.exiting =>
                {
                    self.hidden = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.server_action = match event {
                        tray::TrayEvent::StartServer => Some(ServerAction::Start),
                        tray::TrayEvent::StopServer => Some(ServerAction::Stop),
                        _ => self.server_action,
                    };
                }
                tray::TrayEvent::Exit if guard_exit => {
                    self.hidden = false;
                    self.server_action = Some(ServerAction::Exit);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                tray::TrayEvent::Exit => {
                    self.exiting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => {}
            }
        }
        if self.exiting {
            return true;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !matches!(self.server_action, Some(ServerAction::Exit)) {
                self.hide(ctx);
            }
        }
        if self.hidden && self.hidden_start_frames > 0 {
            self.hide(ctx);
            self.hidden_start_frames -= 1;
            if self.hidden_start_frames > 0 {
                // eframe shows its root after the initial paint.
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        } else {
            self.hidden_start_frames = 0;
        }
        self.hidden
    }
}

struct ControlsState {
    data_dir: Option<PathBuf>,
    queue: Option<QueueView>,
    next_load: Instant,
    #[cfg(windows)]
    bridge_online: bool,
    #[cfg(windows)]
    bridge_probe: Option<Receiver<bool>>,
    #[cfg(windows)]
    update_available_here: bool,
}

struct Installer {
    step: Step,
    python: Option<PythonReadiness>,
    selected_python: Option<PathBuf>,
    installation_dir: Option<PathBuf>,
    install_assets: asset_ui::AssetEditor,
    model_folder: Option<PathBuf>,
    python_check_failed: bool,
    #[cfg(windows)]
    python_install_offer: bool,
    #[cfg(windows)]
    python_installing: bool,
    #[cfg(windows)]
    python_install_error: Option<String>,
    settings: Settings,
    check: Option<CheckOutcome>,
    worker: Option<Receiver<Event>>,
    progress: String,
    install_error: Option<String>,
    install_cancel: Option<Arc<AtomicBool>>,
    installed: Option<Installed>,
    copied: Option<&'static str>,
    panel_connect: bool,
    confirm_rotation: bool,
    rotation_message: Option<String>,
    instances: Vec<discovery::Instance>,
    searching: bool,
    discovery_done: bool,
    manual_confirmed: bool,
    gaming_shortcut: bool,
    gaming_error: Option<String>,
    shortcut_message: Option<(String, bool)>,
    running: Option<RunningBridge>,
    verification: Option<Result<serde_json::Value, String>>,
    verify_claude: bool,
    verify_codex: bool,
    codex_profile: String,
    queue_dialog: Option<QueueDialog>,
    controls: Option<ControlsState>,
    #[cfg(windows)]
    tray_panel: Option<TrayPanel>,
    #[cfg(windows)]
    tray_error: Option<String>,
    #[cfg(windows)]
    startup_setting: Option<StartupSetting>,
    #[cfg(windows)]
    appearance: egui::ThemePreference,
    #[cfg(windows)]
    monitor_error: Option<String>,
    #[cfg(windows)]
    update: update_ui::UpdateDialog,
}

impl Installer {
    fn new(ctx: &egui::Context, mode: LaunchMode) -> Self {
        let data_dir = match &mode {
            LaunchMode::Controls {
                target: MonitorTarget::Installation(directory),
                ..
            } => Some(directory.as_path()),
            _ => None,
        };
        let appearance = theme::load(data_dir);
        theme::configure(ctx, appearance);
        let mut installer = Self {
            step: Step::Prerequisites,
            python: None,
            selected_python: None,
            installation_dir: None,
            install_assets: asset_ui::AssetEditor::default(),
            model_folder: None,
            python_check_failed: false,
            #[cfg(windows)]
            python_install_offer: false,
            #[cfg(windows)]
            python_installing: false,
            #[cfg(windows)]
            python_install_error: None,
            settings: setup::default_settings(),
            check: None,
            worker: None,
            progress: String::new(),
            install_error: None,
            install_cancel: None,
            installed: None,
            copied: None,
            panel_connect: false,
            confirm_rotation: false,
            rotation_message: None,
            instances: Vec::new(),
            searching: false,
            discovery_done: false,
            manual_confirmed: false,
            gaming_shortcut: false,
            gaming_error: None,
            shortcut_message: None,
            running: None,
            verification: None,
            verify_claude: true,
            verify_codex: true,
            codex_profile: String::new(),
            queue_dialog: None,
            controls: None,
            #[cfg(windows)]
            tray_panel: None,
            #[cfg(windows)]
            tray_error: None,
            #[cfg(windows)]
            startup_setting: None,
            #[cfg(windows)]
            appearance,
            #[cfg(windows)]
            monitor_error: None,
            #[cfg(windows)]
            update: update_ui::UpdateDialog::default(),
        };
        match mode {
            LaunchMode::Setup => {
                if let Ok(data) = setup::data_dir() {
                    installer.install_assets.load(&data);
                }
                installer.check_python(ctx);
            }
            LaunchMode::Controls { target, .. } => {
                #[cfg(windows)]
                let update_available_here = matches!(&target, MonitorTarget::Installation(data)
                    if update_handoff::is_installed_monitor(data));
                let (data_dir, queue) = match target {
                    MonitorTarget::Installation(directory) => (Some(directory), None),
                    MonitorTarget::ComfyUrl(url) => {
                        (None, Some(QueueView::with_progress(url, ctx)))
                    }
                    MonitorTarget::Missing => (None, None),
                };
                installer.step = Step::Connect;
                installer.controls = Some(ControlsState {
                    data_dir,
                    queue,
                    next_load: Instant::now(),
                    #[cfg(windows)]
                    bridge_online: false,
                    #[cfg(windows)]
                    bridge_probe: None,
                    #[cfg(windows)]
                    update_available_here,
                });
            }
        }
        installer
    }

    fn change_step(&mut self, step: Step) {
        self.step = step;
        self.copied = None;
    }

    fn open_existing(&mut self, installed: Installed) {
        if let Ok(settings) = setup::saved_install_settings(&installed) {
            self.settings = settings;
        }
        self.installation_dir = Some(installed.data_dir.clone());
        self.install_assets.load(&installed.data_dir);
        self.install_error = None;
        self.installed = Some(installed);
        self.change_step(Step::Connect);
    }

    fn settings_changed(&mut self) {
        self.check = None;
        self.install_error = None;
        self.manual_confirmed = false;
    }

    fn can_continue(&self) -> bool {
        matches!(self.check, Some(CheckOutcome::Models(_, true, _)))
            || (self.manual_confirmed
                && matches!(
                    self.check,
                    Some(CheckOutcome::Models(_, false, _) | CheckOutcome::Offline(_))
                ))
    }

    fn python_ready(&self) -> bool {
        matches!(self.python, Some(PythonReadiness::Ready { .. }))
    }

    fn show_jobs(&mut self, action: QueueAction) {
        let url = if self.step == Step::Comfy {
            Some(self.settings.comfy_url.clone())
        } else {
            self.running
                .as_ref()
                .map(|running| running.backend_url().to_owned())
                .or_else(|| self.installed.as_ref().and_then(setup::installed_comfy_url))
        };
        self.queue_dialog = Some(QueueDialog {
            queue: QueueView::new(url.unwrap_or_default()),
            action,
        });
    }

    #[cfg(windows)]
    fn request_server_action(&mut self, ctx: &egui::Context, action: ServerAction) {
        if self.worker.is_some() || self.update.blocks_controls() {
            return;
        }
        if let Some(setting) = &mut self.startup_setting {
            setting.open = false;
        }
        match action {
            ServerAction::Start => {
                self.queue_dialog = None;
                self.start_panel_server(ctx);
            }
            action @ (ServerAction::Stop | ServerAction::Exit) => {
                if self.running.as_mut().is_some_and(RunningBridge::is_running) {
                    self.show_jobs(match action {
                        ServerAction::Exit => QueueAction::CloseWindow,
                        _ => QueueAction::StopBridge,
                    });
                }
            }
        }
    }

    fn jobs_dialog(&mut self, ctx: &egui::Context) {
        let in_panel = self.controls.is_some();
        let Some(dialog) = &mut self.queue_dialog else {
            return;
        };
        let action = dialog.action;
        let exiting_panel = in_panel && action == QueueAction::CloseWindow;
        let response = panel_modal(ctx, "comfyui-jobs").show(ctx, |ui| {
            if in_panel {
                ui.spacing_mut().item_spacing.y = 6.0;
            }
            ui.set_width((ctx.content_rect().width() - 32.0).clamp(120.0, 420.0));
            let title = match action {
                QueueAction::Inspect => "Jobs",
                _ => "Check jobs",
            };
            ui.heading(RichText::new(title).size(25.0).strong());
            dialog.queue.show(ui);
            if action == QueueAction::Inspect {
                return (secondary(ui, "Close", true).clicked(), false);
            }
            caption(ui, if exiting_panel {
                "Exiting stops the server started by this panel. ComfyUI and its queue keep running."
            } else {
                "Stopping Render Bridge can interrupt job tracking. ComfyUI and its queue keep running."
            });
            let (stop_label, force_label) = if exiting_panel {
                ("Stop and exit", "Stop and exit anyway")
            } else if in_panel {
                ("Stop server", "Stop server anyway")
            } else {
                ("Stop Bridge", "Stop Bridge anyway")
            };
            let label = dialog.queue.stop_confirmation(stop_label, force_label);
            let mut keep = false;
            let mut stop = false;
            ui.horizontal_wrapped(|ui| {
                keep = if in_panel {
                    monitor_button(ui, "Keep running", true, SURFACE)
                } else {
                    secondary(ui, "Keep Bridge running", true)
                }.clicked();
                let label_text = label.unwrap_or(stop_label);
                stop = if in_panel {
                    monitor_button(ui, label_text, label.is_some(), ERROR)
                } else {
                    primary(ui, label_text, label.is_some())
                }.clicked();
            });
            (keep, stop)
        });
        let (dismiss, stop) = response.inner;
        if dismiss || stop || response.should_close() {
            self.queue_dialog = None;
        }
        if stop {
            self.running = None;
            self.verification = None;
            #[cfg(windows)]
            if let Some(controls) = &mut self.controls {
                controls.bridge_online = false;
                controls.bridge_probe = None;
                controls.next_load = Instant::now();
            }
            if action == QueueAction::CloseWindow {
                #[cfg(windows)]
                if let Some(tray) = &mut self.tray_panel {
                    tray.exiting = true;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn check_python(&mut self, ctx: &egui::Context) {
        let selected = self.selected_python.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.python = None;
        self.python_check_failed = false;
        #[cfg(windows)]
        {
            self.python_install_error = None;
        }
        if thread::Builder::new()
            .name("check-python".into())
            .spawn(move || {
                let status = setup::check_python(selected.as_deref());
                let _ = sender.send(Event::PythonChecked(status));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.worker = Some(receiver);
        } else {
            self.python_check_failed = true;
        }
    }

    #[cfg(windows)]
    fn start_python_install(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.python_install_error = None;
        if thread::Builder::new()
            .name("install-python-prerequisite".into())
            .spawn(move || {
                let result =
                    python_install::install_latest_python().map(|()| setup::check_python(None));
                let _ = sender.send(Event::PythonInstallFinished(result));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.worker = Some(receiver);
            self.python_installing = true;
        } else {
            self.python_install_error = Some("Could not open the Windows Python installer. Use the official Python download below.".into());
        }
    }

    fn start_discovery(&mut self, ctx: &egui::Context) {
        let url = self.settings.comfy_url.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.instances.clear();
        self.searching = true;
        self.discovery_done = false;
        if thread::Builder::new()
            .name("find-comfyui".into())
            .spawn(move || {
                let _ = sender.send(Event::Discovered(discovery::discover(&url)));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.worker = Some(receiver);
        } else {
            self.searching = false;
            self.check = Some(CheckOutcome::Failed(
                "Could not start detection. Try again.".into(),
            ));
        }
    }

    fn use_instance(&mut self, index: usize) {
        let instance = &self.instances[index];
        self.settings.comfy_url = instance.url.clone();
        self.settings.comfy_root = instance
            .root
            .as_deref()
            .or_else(|| instance.input.parent())
            .unwrap_or(&instance.input)
            .to_string_lossy()
            .into_owned();
        self.settings.comfy_input = instance
            .root
            .is_none()
            .then(|| instance.input.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.settings.comfy_output = instance
            .root
            .is_none()
            .then(|| instance.output.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.settings_changed();
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        let settings = self.settings.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.check = None;
        self.manual_confirmed = false;
        match thread::Builder::new()
            .name("check-comfyui".into())
            .spawn(move || {
                let result = match setup::validate_settings(&settings) {
                    Ok(settings) => match setup::probe(&settings) {
                        Ok(result) => {
                            match setup::verify_folders(&settings, result.reported.as_ref()) {
                                Ok(verified) => {
                                    CheckOutcome::Models(result.models, verified, result.hardware)
                                }
                                Err(message) => CheckOutcome::Failed(message),
                            }
                        }
                        Err(message) => CheckOutcome::Offline(message),
                    },
                    Err(message) => CheckOutcome::Failed(message),
                };
                let _ = sender.send(Event::Checked(result));
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(receiver),
            Err(_) => {
                self.check = Some(CheckOutcome::Failed(
                    "Could not start the ComfyUI check. Try again.".into(),
                ));
            }
        }
    }

    fn start_verification(&mut self, ctx: &egui::Context) {
        let Some(installed) = self.installed.clone() else {
            return;
        };
        self.verification = None;
        self.progress = "Starting the Bridge and checking authenticated MCP…".into();
        let mut running = self.running.take();
        let codex_profile = self.codex_profile.trim().to_owned();
        let clients: Vec<_> = [
            (Client::Claude, self.verify_claude),
            (Client::Codex, self.verify_codex),
        ]
        .into_iter()
        .filter_map(|(client, selected)| selected.then_some(client))
        .collect();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        if thread::Builder::new()
            .name("verify-mcp".into())
            .spawn(move || {
                let result = connection::start_and_verify(
                    &installed,
                    &mut running,
                    &clients,
                    &codex_profile,
                );
                let _ = sender.send(Event::Verified(running, result));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.worker = Some(receiver);
        } else {
            self.verification = Some(Err(
                "Could not start the connection check. Try again.".into()
            ));
        }
    }

    fn rotate_token(&mut self, ctx: &egui::Context, installed: Installed) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("rotate-render-bridge-token".into())
            .spawn(move || {
                let _ = sender.send(Event::TokenRotated(setup::rotate_token(&installed)));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.worker = Some(receiver);
            self.progress = "Rotating the private token…".into();
            self.rotation_message = None;
        } else {
            self.rotation_message = Some("Could not start token rotation.".into());
        }
    }

    fn change_gaming_mode(&mut self, ctx: &egui::Context, enabled: bool) {
        if self.controls.as_ref().is_some_and(|controls| {
            controls
                .data_dir
                .as_deref()
                .and_then(setup::existing_install_at)
                .as_ref()
                .map(|current| &current.app_dir)
                != self.installed.as_ref().map(|opened| &opened.app_dir)
        }) {
            self.load_controls(ctx);
            self.gaming_error = Some(
                "The active setup changed. Review the updated queue before using Gaming Mode."
                    .into(),
            );
            return;
        }
        let Some(installed) = self.installed.clone() else {
            return;
        };
        self.gaming_error = None;
        self.progress = "Updating Gaming Mode…".into();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let result = thread::Builder::new()
            .name("render-bridge-gaming-mode".into())
            .spawn(move || {
                let state = if enabled { "on" } else { "off" };
                let succeeded = setup::quiet_command(setup::venv_python(&installed.app_dir))
                    .arg(installed.app_dir.join("setup_render_bridge.py"))
                    .args(["--data-dir"])
                    .arg(&installed.data_dir)
                    .args(["gaming-mode", state])
                    .current_dir(&installed.app_dir)
                    .status()
                    .is_ok_and(|status| status.success());
                let result = succeeded.then_some(()).ok_or_else(|| {
                    "Could not change Gaming Mode. Check the installation and try again.".into()
                });
                let _ = sender.send(Event::GamingModeChanged(result));
                ctx.request_repaint();
            });
        if result.is_ok() {
            self.worker = Some(receiver);
        } else {
            self.gaming_error = Some("Could not start the Gaming Mode control.".into());
        }
    }

    #[cfg(windows)]
    fn start_panel_server(&mut self, ctx: &egui::Context) {
        let Some(installed) = self.installed.clone() else {
            return;
        };
        let already_running = || serde_json::json!({ "bridge": "already_running" });
        if self.running.as_mut().is_some_and(RunningBridge::is_running) {
            self.verification = Some(Ok(already_running()));
            return;
        }
        let confirm_only = self
            .controls
            .as_ref()
            .is_some_and(|controls| controls.bridge_online);
        self.verification = None;
        self.progress = "Checking the Render Bridge server…".into();
        self.running = None;
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("render-bridge-panel-start".into())
            .spawn(move || {
                let mut running = None;
                let result = if connection::bridge_is_listening(&installed) {
                    Ok(already_running())
                } else if confirm_only {
                    Err("The server no longer responds. Check the status and try again.".into())
                } else {
                    connection::start_and_verify(&installed, &mut running, &[], "")
                };
                let _ = sender.send(Event::Verified(running, result));
                ctx.request_repaint();
            });
        if worker.is_ok() {
            self.worker = Some(receiver);
        } else {
            self.verification = Some(Err("Could not check the Render Bridge server.".into()));
        }
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        let settings = self.settings.clone();
        let selected_python = self.selected_python.clone();
        let selected_data_dir = self.installation_dir.clone();
        let folders = self.install_assets.folders.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.install_error = None;
        self.progress = "Preparing installation…".into();
        match thread::Builder::new()
            .name("install-render-bridge".into())
            .spawn(move || {
                let result = (|| {
                    let data = selected_data_dir.clone().map_or_else(setup::data_dir, Ok)?;
                    asset_folders::validate(
                        &setup::folder(&settings, "output"),
                        &setup::folder(&settings, "input"),
                        &data,
                        &folders,
                    )?;
                    let installed = setup::install(
                        &settings,
                        selected_python.as_deref(),
                        selected_data_dir.as_deref(),
                        &worker_cancel,
                        |message| {
                            let _ = sender.send(Event::Progress(message.into()));
                            ctx.request_repaint();
                        },
                    )?;
                    asset_folders::save(&installed.data_dir, &folders)?;
                    Ok(installed)
                })();
                let _ = sender.send(Event::Finished(result));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.install_cancel = Some(cancel);
                self.worker = Some(receiver);
            }
            Err(_) => self.install_error = Some("Could not start installation. Try again.".into()),
        }
    }

    fn receive_updates(&mut self) {
        while let Some(receiver) = &self.worker {
            match receiver.try_recv() {
                Ok(Event::PythonChecked(status)) => {
                    self.python = Some(status);
                    self.worker = None;
                }
                #[cfg(windows)]
                Ok(Event::PythonInstallFinished(result)) => {
                    self.worker = None;
                    self.python_installing = false;
                    match result {
                        Ok(readiness) => {
                            self.selected_python = None;
                            self.python = Some(readiness);
                        }
                        Err(message) => self.python_install_error = Some(message),
                    }
                }
                Ok(Event::Checked(result)) => {
                    self.check = Some(result);
                    self.worker = None;
                }
                Ok(Event::Discovered(instances)) => {
                    self.worker = None;
                    self.searching = false;
                    self.discovery_done = true;
                    self.instances = instances;
                    if self.instances.len() == 1 {
                        self.use_instance(0);
                    }
                }
                Ok(Event::Verified(running, result)) => {
                    self.worker = None;
                    self.running = running;
                    #[cfg(windows)]
                    if let Some(controls) = &mut self.controls {
                        controls.bridge_online = result.as_ref().is_ok_and(|result| {
                            matches!(
                                result["bridge"].as_str(),
                                Some("ok" | "already_running" | "comfyui_unavailable")
                            )
                        });
                        controls.bridge_probe = None;
                        controls.next_load = Instant::now();
                    }
                    self.verification = Some(result);
                }
                Ok(Event::GamingModeChanged(result)) => {
                    self.worker = None;
                    self.gaming_error = result.err();
                }
                Ok(Event::TokenRotated(result)) => {
                    self.worker = None;
                    self.rotation_message = Some(result.map_or_else(
                        |error| error,
                        |()| "Token rotated. Start Render Bridge and reconnect your agents using the updated file.".into(),
                    ));
                }
                Ok(Event::Progress(message)) => self.progress = message,
                Ok(Event::Finished(result)) => {
                    self.worker = None;
                    self.install_cancel = None;
                    match result {
                        Ok(installed) => {
                            #[cfg(windows)]
                            {
                                self.monitor_error =
                                    shortcuts::create_monitor_shortcut(&installed).err();
                            }
                            if self.gaming_shortcut {
                                self.shortcut_message =
                                    Some(match shortcuts::create_gaming_shortcut(&installed) {
                                        Ok(path) => (
                                            format!(
                                                "Gaming Mode shortcut created at {}.",
                                                path.display()
                                            ),
                                            true,
                                        ),
                                        Err(message) => (message, false),
                                    });
                            }
                            self.installed = Some(installed);
                            self.change_step(Step::Connect);
                        }
                        Err(message) => self.install_error = Some(message),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.install_cancel = None;
                    self.searching = false;
                    let message = "Setup stopped unexpectedly. You can try this step again.".into();
                    if self.step == Step::Prerequisites {
                        self.python_check_failed = true;
                        #[cfg(windows)]
                        {
                            self.python_installing = false;
                        }
                    } else if self.step == Step::Install {
                        self.install_error = Some(message);
                    } else if self.step == Step::Connect {
                        self.verification = Some(Err(message));
                    } else {
                        self.check = Some(CheckOutcome::Failed(message));
                    }
                }
            }
        }
    }

    fn rail(&self, ctx: &egui::Context) {
        egui::SidePanel::left("setup-steps")
            .exact_width(230.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, RAIL))
                    .inner_margin(egui::Margin::symmetric(24, 31)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    bridge_mark(ui);
                    ui.label(
                        RichText::new("Render\nBridge")
                            .size(27.0)
                            .strong()
                            .color(CREAM),
                    );
                });
                ui.add_space(37.0);
                for (step, number, label) in STEPS {
                    let selected = step == self.step;
                    let background = if selected {
                        theme::color(ctx, RAIL_ACTIVE)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let foreground = if selected {
                        theme::color(ctx, INK)
                    } else {
                        theme::color(ctx, RAIL_TEXT)
                    };
                    Frame::new()
                        .fill(background)
                        .corner_radius(13)
                        .inner_margin(egui::Margin::symmetric(12, 12))
                        .show(ui, |ui| {
                            ui.set_min_width(151.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(number)
                                        .monospace()
                                        .size(13.0)
                                        .color(foreground),
                                );
                                ui.add_space(5.0);
                                ui.label(
                                    RichText::new(label).size(15.0).strong().color(foreground),
                                );
                            });
                        });
                }
            });
    }

    fn prerequisites_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Get ready to render",
            "Set up a clean machine with Python and ComfyUI. Choose an agent when you are ready to connect.",
        );
        ui.hyperlink_to("Privacy and network use", PRIVACY_GUIDE);
        let idle = self.worker.is_none();
        let ready = self.python_ready();
        card(ui, |ui| {
            field_label(ui, "Python 3.11 or newer");
            if self.python_check_failed {
                caption(ui, "Python could not be checked. Try the check again.");
            } else if !idle {
                #[cfg(windows)]
                let installing = self.python_installing;
                #[cfg(not(windows))]
                let installing = false;
                activity(
                    ui,
                    if installing {
                        "Waiting for Windows Python setup to finish…"
                    } else {
                        "Checking Python and virtual environment support…"
                    },
                );
            } else if let Some(readiness) = &self.python {
                match readiness {
                    PythonReadiness::Ready {
                        version,
                        executable,
                    } => {
                        ui.label(
                            RichText::new(format!("Python {version} is ready"))
                                .strong()
                                .color(theme::color(ui.ctx(), GREEN)),
                        );
                        code_label(ui, &executable.to_string_lossy());
                    }
                    PythonReadiness::TooOld { version } => {
                        ui.label(
                            RichText::new(format!("Python {version} is too old"))
                                .strong()
                                .color(theme::color(ui.ctx(), CAUTION)),
                        );
                        caption(ui, "Install Python 3.11 or newer, then check again.");
                    }
                    PythonReadiness::MissingVenv { version } => {
                        ui.label(
                            RichText::new(format!(
                                "Python {version} needs virtual environment support"
                            ))
                            .strong()
                            .color(theme::color(ui.ctx(), CAUTION)),
                        );
                        caption(ui, python_venv_help());
                    }
                    PythonReadiness::Missing => {
                        ui.label(
                            RichText::new("Python was not found")
                                .strong()
                                .color(theme::color(ui.ctx(), CAUTION)),
                        );
                        caption(ui, "Install Python 3.11 or newer, then check again.");
                    }
                    PythonReadiness::InvalidSelection => {
                        ui.label(
                            RichText::new("This file could not be verified as Python")
                                .strong()
                                .color(theme::color(ui.ctx(), CAUTION)),
                        );
                        caption(
                            ui,
                            "Select the Python executable from an installation, or use automatic detection.",
                        );
                    }
                }
            }
            if !ready {
                caption(ui, "Python 3.13 is recommended for a new installation.");
                #[cfg(windows)]
                if setup::winget_available()
                    && secondary(ui, "Install Python with WinGet", idle).clicked()
                {
                    self.python_install_offer = true;
                }
                let (label, url) = python_install_link();
                ui.hyperlink_to(label, url);
                #[cfg(target_os = "linux")]
                if secondary(ui, "Copy Debian or Ubuntu command", true).clicked() {
                    ui.ctx().copy_text(
                        "sudo apt update && sudo apt install python3 python3-venv".into(),
                    );
                    self.copied =
                        Some("Run the copied command in a terminal, then check Python again.");
                }
            }
            #[cfg(windows)]
            if let Some(message) = &self.python_install_error {
                caption(ui, message);
            }
            ui.horizontal_wrapped(|ui| {
                if secondary(ui, "Select Python executable", idle).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Select your Python executable")
                        .pick_file()
                {
                    self.selected_python = Some(path);
                    self.check_python(ui.ctx());
                }
                if self.selected_python.is_some()
                    && secondary(ui, "Use automatic detection", idle).clicked()
                {
                    self.selected_python = None;
                    self.check_python(ui.ctx());
                } else if ready && secondary(ui, "Check Python again", idle).clicked() {
                    self.check_python(ui.ctx());
                }
            });
            if let Some(message) = self.copied {
                caption(ui, message);
            }
        });
        card(ui, |ui| {
            field_label(ui, "ComfyUI on this computer");
            caption(
                ui,
                "Install Comfy Desktop, finish its setup, and start a local build. The next step checks its API and folders. You can also use a portable or manual installation.",
            );
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Get Comfy Desktop", COMFY_DOWNLOAD);
                ui.hyperlink_to("Manual ComfyUI setup", COMFY_GUIDE);
            });
        });
        card(ui, |ui| {
            field_label(ui, "Agent client");
            caption(
                ui,
                "Install and sign in to a client before you connect it. You can set up the Bridge first.",
            );
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Set up Claude Code", CLAUDE_SETUP);
                ui.hyperlink_to("Set up Codex", CODEX_SETUP);
            });
        });
        ui.horizontal_wrapped(|ui| {
            if let Some(installed) = setup::existing_install()
                && secondary(ui, "Open existing setup", idle).clicked()
            {
                self.open_existing(installed);
            }
            if secondary(ui, "Browse for an installation", idle).clicked()
                && let Some(folder) = rfd::FileDialog::new()
                    .set_title("Choose an existing Render Bridge installation folder")
                    .pick_folder()
            {
                if let Some(installed) = setup::existing_install_at(&folder) {
                    self.open_existing(installed);
                } else {
                    self.install_error =
                        Some("That folder does not contain a verified Render Bridge setup.".into());
                }
            }
        });
        if let Some(error) = &self.install_error {
            caption(ui, error);
        }
        #[cfg(windows)]
        if self.python_install_offer {
            let prompt = egui::Modal::new(egui::Id::new("python-install-approval")).show(ui.ctx(), |ui| {
                ui.set_width(340.0);
                ui.heading("Install Python on this PC?");
                caption(ui, "WinGet will request the latest available Python 3.13 patch for all users on this PC. Windows will ask for administrator approval.");
                caption(ui, "You can cancel and use the official Python download instead.");
                let mut approve = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    cancel = secondary(ui, "Cancel", true).clicked();
                    approve = primary(ui, "Request administrator approval", true).clicked();
                });
                (approve, cancel)
            });
            let (approve, cancel) = prompt.inner;
            if approve || cancel || prompt.should_close() {
                self.python_install_offer = false;
            }
            if approve {
                self.start_python_install(ui.ctx());
            }
        }
    }

    fn comfy_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Connect to ComfyUI",
            "Find a running ComfyUI instance or enter the address and folders for its build.",
        );
        let idle = self.worker.is_none();
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Detect running ComfyUI", idle).clicked() {
                self.start_discovery(ui.ctx());
            }
            if secondary(ui, "ComfyUI Jobs", idle).clicked() {
                self.show_jobs(QueueAction::Inspect);
            }
        });
        if !self.instances.is_empty() {
            let mut chosen = None;
            card(ui, |ui| {
                field_label(ui, "Detected ComfyUI instances");
                for (index, instance) in self.instances.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        if secondary(ui, &format!("Use {}", instance.url), idle).clicked() {
                            chosen = Some(index);
                        }
                    });
                    caption(ui, &format!("Input: {}", instance.input.display()));
                    caption(ui, &format!("Output: {}", instance.output.display()));
                }
            });
            if let Some(index) = chosen {
                self.use_instance(index);
            }
        } else if self.discovery_done {
            caption(
                ui,
                "No running instance reported its folders. Start ComfyUI or enter the build's address and folders.",
            );
        }
        card(ui, |ui| {
            ui.add_enabled_ui(idle, |ui| {
                let root_label = field_label(ui, "ComfyUI folder");
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 108.0).max(150.0);
                    let mut root_changed =
                        text_edit(ui, &mut self.settings.comfy_root, width, root_label);
                    if secondary(ui, "Browse…", true).clicked() {
                        let mut dialog =
                            rfd::FileDialog::new().set_title("Choose the ComfyUI folder");
                        let current = Path::new(&self.settings.comfy_root);
                        if current.is_dir() {
                            dialog = dialog.set_directory(current);
                        }
                        if let Some(path) = dialog.pick_folder() {
                            self.settings.comfy_root = path.to_string_lossy().into_owned();
                            root_changed = true;
                        }
                    }
                    if root_changed {
                        self.settings.comfy_input.clear();
                        self.settings.comfy_output.clear();
                        changed = true;
                    }
                });
                caption(ui, "Select the folder that contains input and output.");
                ui.add_space(9.0);
                let url_label = field_label(ui, "Local API address");
                let url_width = ui.available_width();
                changed |= text_edit(ui, &mut self.settings.comfy_url, url_width, url_label);
                ui.collapsing("Separate input and output folders", |ui| {
                    let input = field_label(ui, "Input folder override");
                    changed |= text_edit(
                        ui,
                        &mut self.settings.comfy_input,
                        ui.available_width(),
                        input,
                    );
                    let output = field_label(ui, "Output folder override");
                    changed |= text_edit(
                        ui,
                        &mut self.settings.comfy_output,
                        ui.available_width(),
                        output,
                    );
                    caption(
                        ui,
                        "Leave overrides empty to use input and output under the main folder.",
                    );
                });
            });
        });
        if changed {
            self.settings_changed();
        }
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            if self.can_continue() && secondary(ui, "Check again", idle).clicked() {
                self.start_check(ui.ctx());
            }
            ui.hyperlink_to("Get Comfy Desktop", COMFY_DOWNLOAD);
            ui.hyperlink_to("Manual ComfyUI setup", COMFY_GUIDE);
            ui.hyperlink_to(
                "Find your Comfy Desktop port and folders",
                "https://docs.comfy.org/installation/desktop/usage/instance-management",
            );
        });
        if self.worker.is_some() {
            activity(
                ui,
                if self.searching {
                    "Finding running ComfyUI instances…"
                } else {
                    "Checking the local API and model loaders…"
                },
            );
        }
        if let Some(result) = &self.check {
            match result {
                CheckOutcome::Models(_, true, _) => status(
                    ui,
                    "ComfyUI instance verified",
                    "The running API matched the selected input and output folders.",
                    GREEN,
                ),
                CheckOutcome::Models(_, false, _) => {
                    status(
                        ui,
                        "ComfyUI folder pairing unverified",
                        "The API responded but did not report enough information to identify its folders.",
                        CAUTION,
                    );
                    ui.checkbox(
                        &mut self.manual_confirmed,
                        "I checked these folders against the running ComfyUI build",
                    );
                }
                CheckOutcome::Offline(message) => {
                    status(ui, "Could not verify ComfyUI", message, CAUTION);
                    ui.checkbox(
                        &mut self.manual_confirmed,
                        "Continue with these folders while ComfyUI is offline",
                    );
                }
                CheckOutcome::Failed(message) => {
                    status(ui, "Check your setup details", message, ERROR)
                }
            }
        }
    }

    fn models_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Check starter models",
            "Render Bridge uses model files from your ComfyUI installation.",
        );
        match &self.check {
            _ if self.worker.is_some() => activity(ui, "Checking the model loaders…"),
            Some(CheckOutcome::Offline(message)) => {
                status(ui, "Model status unavailable", message, CAUTION)
            }
            Some(CheckOutcome::Failed(message)) => {
                status(ui, "Check your setup details", message, ERROR)
            }
            _ => {}
        }
        if let Some(CheckOutcome::Models(_, _, Some(hardware))) = &self.check {
            card(ui, |ui| {
                field_label(ui, "Detected render hardware");
                caption(ui, &hardware.summary());
                caption(ui, hardware.recommendation());
            });
        }
        card(ui, |ui| {
            asset_ui::folder_option(
                ui,
                "Model storage folder",
                &mut self.model_folder,
                "Use your ComfyUI models folder, or select a folder on another drive.",
            );
            if let Some(folder) = &self.model_folder
                && secondary(ui, "Copy ComfyUI model paths", true).clicked()
            {
                ui.ctx().copy_text(model_paths(folder));
            }
            ui.hyperlink_to(
                "Set up a separate model drive",
                format!("{MODEL_GUIDE}#store-models-on-another-drive"),
            );
        });
        let checks = match &self.check {
            Some(CheckOutcome::Models(checks, _, _)) => checks.as_slice(),
            _ => &[],
        };
        let file_status = |filename| {
            models::FileStatus::from(
                checks
                    .iter()
                    .find(|check| check.filename == filename)
                    .and_then(|check| check.available),
            )
        };
        for pipeline in models::STARTER_PIPELINES {
            card(ui, |ui| {
                field_label(ui, pipeline.name);
                let (label, color) = model_status(
                    models::pipeline_status(
                        pipeline.files.iter().map(|file| file_status(file.filename)),
                    ),
                    true,
                );
                ui.label(
                    RichText::new(label)
                        .strong()
                        .color(theme::color(ui.ctx(), color)),
                );
                caption(ui, pipeline.description);
                for file in pipeline.files {
                    ui.separator();
                    let (label, color) = model_status(file_status(file.filename), false);
                    ui.label(
                        RichText::new(label)
                            .strong()
                            .color(theme::color(ui.ctx(), color)),
                    );
                    ui.add(egui::Label::new(RichText::new(file.filename).monospace()).wrap());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(file.folder)
                                .monospace()
                                .color(theme::color(ui.ctx(), MUTED)),
                        );
                        ui.hyperlink_to("Open Hugging Face file", file.source);
                    });
                }
            });
        }
        ui.add_space(6.0);
        caption(
            ui,
            "These starter files are public and have no account requirement. Add them to the selected ComfyUI build's model folders, refresh its model list, then check again. File checks do not measure GPU compatibility.",
        );
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Check again", self.worker.is_none()).clicked() {
                self.start_check(ui.ctx());
            }
            ui.hyperlink_to("Open model and pipeline guide", MODEL_GUIDE);
        });
    }

    fn install_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Install Render Bridge",
            "Install the bridge for your user account with a dedicated Python environment.",
        );
        card(ui, |ui| {
            field_label(ui, "Local API address");
            code_label(ui, &self.settings.comfy_url);
            ui.separator();
            field_label(ui, "ComfyUI input folder");
            code_label(
                ui,
                &setup::folder(&self.settings, "input").to_string_lossy(),
            );
            field_label(ui, "ComfyUI output folder");
            code_label(
                ui,
                &setup::folder(&self.settings, "output").to_string_lossy(),
            );
            ui.separator();
            ui.add_enabled_ui(self.worker.is_none(), |ui| {
                let default = setup::data_dir().ok();
                if asset_ui::folder_option(
                    ui,
                    "Render Bridge installation folder",
                    &mut self.installation_dir,
                    &default.as_ref().map_or_else(
                        || "Default user folder".into(),
                        |path| path.to_string_lossy().into_owned(),
                    ),
                ) && let Some(data) = self.installation_dir.as_ref().or(default.as_ref())
                {
                    self.install_assets.load(data);
                }
                caption(
                    ui,
                    "Use a folder only you can access for the runtime and private settings.",
                );
                self.install_assets.fields(ui);
                if let Some(error) = &self.install_assets.message {
                    caption(ui, error);
                }
            });
        });
        ui.add_space(9.0);
        caption(
            ui,
            "Install uses Python 3.11 or newer to download the bridge dependencies. The next step shows how to connect your MCP client.",
        );
        if self.worker.is_some() {
            ui.add_space(9.0);
            let cancelling = self
                .install_cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
            activity(
                ui,
                if cancelling {
                    "Cancelling setup…"
                } else {
                    &self.progress
                },
            );
            caption(
                ui,
                "Setup activates the new runtime after its dependency and startup checks pass.",
            );
        }
        ui.add_enabled_ui(self.worker.is_none(), |ui| {
            ui.checkbox(&mut self.gaming_shortcut, "Create a Gaming Mode shortcut");
        });
        caption(
            ui,
            "The shortcut toggles whether agents can start new GPU work.",
        );
        if let Some(message) = &self.install_error {
            status(ui, "Installation could not finish", message, ERROR);
        }
    }

    fn connect_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Connect your MCP client",
            "Start Render Bridge, then paste the setup prompt into an agent on this computer.",
        );
        let Some(installed) = self.installed.clone() else {
            return;
        };
        let mut copied = None;
        let idle = self.worker.is_none();
        let mut selection_changed = false;
        copied = copy_agent_prompt(ui, &installed).or(copied);
        card(ui, |ui| {
            ui.add_enabled_ui(idle, |ui| {
                ui.horizontal_wrapped(|ui| {
                    selection_changed |= ui
                        .checkbox(&mut self.verify_claude, "Claude Code")
                        .changed();
                    selection_changed |= ui.checkbox(&mut self.verify_codex, "Codex").changed();
                });
            });
            for (client, selected) in [
                (Client::Claude, self.verify_claude),
                (Client::Codex, self.verify_codex),
            ] {
                if selected && let Some(path) = connection::profile_path(client) {
                    caption(
                        ui,
                        &format!("{} settings: {}", client.label(), path.display()),
                    );
                }
            }
            if self.verify_codex {
                ui.add_enabled_ui(idle, |ui| {
                    let label = field_label(ui, "Codex profile name (optional)");
                    selection_changed |=
                        text_edit(ui, &mut self.codex_profile, ui.available_width(), label);
                });
            }
            caption(
                ui,
                "Setup checks these saved user settings and leaves them intact.",
            );
            if copy_row(ui, "MCP endpoint", "Copy address", &installed.mcp_url) {
                copied = Some("MCP endpoint copied");
            }
            ui.separator();
            if copy_row(
                ui,
                "Private token file path",
                "Copy path",
                &installed.token_path.to_string_lossy(),
            ) {
                copied = Some("Token file path copied");
            }
        });
        if selection_changed {
            self.verification = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("Connect an MCP client", CLIENT_GUIDE);
            ui.hyperlink_to("Connect from another device", REMOTE_GUIDE);
        });
        if self.worker.is_some() {
            activity(ui, &self.progress);
        } else if let Some(verification) = &self.verification {
            match verification {
                Err(message) => status(ui, "Verification could not finish", message, ERROR),
                Ok(result) => {
                    let (message, passed) =
                        bridge_status(result["bridge"].as_str().unwrap_or_default());
                    status(
                        ui,
                        "Render Bridge",
                        message,
                        if passed { GREEN } else { CAUTION },
                    );
                    if result["folders"].as_bool() != Some(true) {
                        status(
                            ui,
                            "ComfyUI folder pairing unverified",
                            "Confirm the running build's folders before using uploads or asset records.",
                            CAUTION,
                        );
                    }
                    for client in [Client::Claude, Client::Codex] {
                        if let Some(value) = result[client.key()].as_str() {
                            let (message, passed) = client_status(value);
                            status(
                                ui,
                                client.label(),
                                message,
                                if passed { GREEN } else { CAUTION },
                            );
                        }
                    }
                }
            }
        }
        if let Some((message, passed)) = &self.shortcut_message {
            status(
                ui,
                "Gaming Mode shortcut",
                message,
                if *passed { GREEN } else { CAUTION },
            );
        }
        self.show_gaming_mode(ui, &installed);
        #[cfg(windows)]
        {
            if secondary(ui, "Open Render Bridge", idle).clicked() {
                self.monitor_error = shortcuts::open_monitor(&installed).err();
            }
            if let Some(message) = &self.monitor_error {
                status(ui, "Render Bridge panel", message, CAUTION);
            }
        }
        if secondary(ui, "ComfyUI Jobs", idle).clicked() {
            self.show_jobs(QueueAction::Inspect);
        }
        caption(
            ui,
            "Stop Bridge or close this window to review the queue and stop the Bridge started here.",
        );
        caption(
            ui,
            "You can turn Gaming Mode off from the commands below without a shortcut.",
        );
        ui.collapsing("Shutdown and terminal commands", |ui| {
            caption(ui, "For a Bridge started in a terminal, press Ctrl+C in that terminal.");
            if copy_row(ui, "Start in a terminal", "Copy start command", &installed.start_command) {
                copied = Some("Start command copied");
            }
            if let Some(command) = installed.start_command.strip_suffix(" start") {
                let gaming_off = format!("{command} gaming-mode off");
                if copy_row(ui, "Turn Gaming Mode off", "Copy off command", &gaming_off) {
                    copied = Some("Gaming Mode off command copied");
                }
                caption(ui, "Run the off command in a terminal to let agents render again. This works without the shortcut.");
                let gaming_status = format!("{command} gaming-mode status");
                if copy_row(ui, "Check Gaming Mode", "Copy status command", &gaming_status) {
                    copied = Some("Gaming Mode status command copied");
                }
            }
        });
        if copied.is_some() {
            self.copied = copied;
        }
        if let Some(message) = self.copied {
            ui.label(
                RichText::new(message)
                    .size(14.0)
                    .color(theme::color(ui.ctx(), GREEN)),
            );
        }
    }

    fn show_gaming_mode(&mut self, ui: &mut Ui, installed: &Installed) {
        if let Some(on) = gaming_mode(installed) {
            if gaming_mode_button(ui, on, self.worker.is_none()).clicked() {
                self.change_gaming_mode(ui.ctx(), !on);
            }
        } else {
            status(
                ui,
                "Gaming Mode",
                "Status unavailable. Run the status command in a terminal.",
                MUTED,
            );
        }
        if let Some(message) = &self.gaming_error {
            status(ui, "Gaming Mode could not change", message, ERROR);
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    fn load_controls(&mut self, ctx: &egui::Context) {
        let Some(controls) = &mut self.controls else {
            return;
        };
        let installed = controls
            .data_dir
            .as_deref()
            .and_then(setup::existing_install_at);
        let url = installed.as_ref().and_then(setup::installed_comfy_url);
        let changed = installed.as_ref().map(|current| &current.app_dir)
            != self.installed.as_ref().map(|opened| &opened.app_dir);
        #[cfg(windows)]
        {
            if changed {
                controls.bridge_probe = None;
                controls.bridge_online = false;
                self.verification = None;
            }
            if let Some(probe) = &controls.bridge_probe {
                match probe.try_recv() {
                    Ok(online) => {
                        if controls.bridge_online && !online {
                            self.verification = None;
                        }
                        controls.bridge_online = online;
                        controls.bridge_probe = None;
                    }
                    Err(TryRecvError::Disconnected) => controls.bridge_probe = None,
                    Err(TryRecvError::Empty) => {}
                }
            }
            if controls.bridge_probe.is_none()
                && self.worker.is_none()
                && let Some(installed) = installed.clone()
            {
                let (sender, receiver) = mpsc::channel();
                if thread::Builder::new()
                    .name("check-render-bridge-status".into())
                    .spawn(move || {
                        let _ = sender.send(connection::bridge_is_listening(&installed));
                    })
                    .is_ok()
                {
                    controls.bridge_probe = Some(receiver);
                }
            }
        }
        match url {
            Some(url)
                if changed
                    || !controls
                        .queue
                        .as_ref()
                        .is_some_and(|queue| queue.is_for(&url)) =>
            {
                controls.queue = Some(QueueView::with_progress(url, ctx));
            }
            None => controls.queue = None,
            _ => {}
        }
        self.installed = installed;
        controls.next_load = Instant::now() + Duration::from_secs(2);
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn controls_screen(&mut self, ctx: &egui::Context) {
        if self.controls.as_ref().is_some_and(|controls| {
            controls.data_dir.is_some() && Instant::now() >= controls.next_load
        }) {
            self.load_controls(ctx);
        }
        egui::TopBottomPanel::top("controls-banner")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, RAIL))
                    .inner_margin(egui::Margin::symmetric(16, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    bridge_mark(ui);
                    ui.label(
                        RichText::new("Render Bridge")
                            .size(20.0)
                            .strong()
                            .color(CREAM),
                    );
                });
            });
        egui::TopBottomPanel::bottom("controls-actions")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    #[cfg(windows)]
                    if let Some(setting) = &mut self.startup_setting
                        && monitor_button(ui, "Settings", true, SURFACE).clicked()
                    {
                        setting.open(self.installed.as_ref());
                    }
                    if self.installed.is_some()
                        && monitor_button(ui, "Connect", true, SURFACE).clicked()
                    {
                        self.panel_connect = true;
                        self.copied = None;
                        self.confirm_rotation = false;
                    }
                    #[cfg(windows)]
                    if self.installed.is_some()
                        && self
                            .controls
                            .as_ref()
                            .is_some_and(|controls| controls.update_available_here)
                        && monitor_button(ui, "Update", true, SURFACE).clicked()
                    {
                        self.update.check(ctx);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        #[cfg(windows)]
                        let close_label = if self.tray_panel.is_some() {
                            "Hide to tray"
                        } else {
                            "Close"
                        };
                        #[cfg(not(windows))]
                        let close_label = "Close";
                        if monitor_button(ui, close_label, true, LIME).clicked() {
                            #[cfg(windows)]
                            if let Some(tray) = &mut self.tray_panel {
                                tray.hide(ctx);
                                return;
                            }
                            if self.running.as_mut().is_some_and(RunningBridge::is_running) {
                                self.show_jobs(QueueAction::CloseWindow);
                                return;
                            }
                            if self.worker.is_some() {
                                return;
                            }
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::color(ctx, CREAM)).inner_margin(egui::Margin::symmetric(16, 6)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 7.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Jobs").size(23.0).strong());
                        #[cfg(windows)]
                        if self.installed.is_some() {
                            let idle = self.worker.is_none();
                            let owned = self.running.as_mut().is_some_and(RunningBridge::is_running);
                            let online = owned || self.controls.as_ref().is_some_and(|controls| controls.bridge_online);
                            ui.add_space(4.0);
                            let start_fill = if online { LINE } else { SURFACE };
                            if monitor_button(ui, "Start server", idle, start_fill)
                                .on_hover_text(if online { "Confirm the server is running" } else { "Start Render Bridge" })
                                .clicked() {
                                self.request_server_action(ctx, ServerAction::Start);
                            }
                            let stop_fill = if online { ERROR } else { LINE };
                            if monitor_button(ui, "Stop server", idle && owned, stop_fill)
                                .on_disabled_hover_text(if online { "This panel can stop the server it starts." } else { "Start the server to enable Stop." })
                                .clicked() {
                                self.request_server_action(ctx, ServerAction::Stop);
                            }
                            let server = match (online, owned) {
                                (true, false) => "Server started elsewhere",
                                (true, true) => "Render Bridge running",
                                _ => "Render Bridge server",
                            };
                            ui.label(RichText::new(server).size(13.0).color(theme::color(ui.ctx(), MUTED)));
                        }
                    });
                    if let Some(queue) = self.controls.as_mut().and_then(|controls| controls.queue.as_mut()) {
                        queue.show(ui);
                    } else {
                        status(ui, "Queue status unavailable",
                            "The saved ComfyUI connection could not be read. Open Render Bridge Setup to review the installation.", CAUTION);
                    }
                    if self.worker.is_some() {
                        activity(ui, &self.progress);
                    }
                    #[cfg(windows)]
                    if let Some(verification) = &self.verification {
                        match verification {
                            Ok(result) => {
                                let (message, passed) = bridge_status(result["bridge"].as_str().unwrap_or_default());
                                status(ui, "Render Bridge", message, if passed { GREEN } else { CAUTION });
                            }
                            Err(message) => status(ui, "Render Bridge server", message, CAUTION),
                        }
                    }
                    if let Some(installed) = self.installed.clone() {
                        self.show_gaming_mode(ui, &installed);
                    }
                    #[cfg(windows)]
                    if let Some(error) = &self.tray_error {
                        status(ui, "Tray icon unavailable", error, CAUTION);
                    }
                });
            });
        if self.panel_connect {
            if let Some(installed) = self.installed.clone() {
                #[cfg(windows)]
                let online = self.running.as_mut().is_some_and(RunningBridge::is_running)
                    || self
                        .controls
                        .as_ref()
                        .is_some_and(|controls| controls.bridge_online);
                #[cfg(not(windows))]
                let online = self.running.as_mut().is_some_and(RunningBridge::is_running);
                let can_rotate = self.worker.is_none() && !online;
                let mut rotate = false;
                let response = panel_modal(ctx, "connect-installed-bridge").show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.heading("Connect an agent");
                    egui::ScrollArea::vertical()
                        .max_height((ctx.content_rect().height() - 116.0).max(136.0))
                        .show(ui, |ui| {
                    caption(ui, "Use Start server in Jobs if Render Bridge is offline, then paste this prompt into your agent.");
                    self.copied = copy_agent_prompt(ui, &installed).or(self.copied);
                    if copy_row(ui, "MCP endpoint", "Copy address", &installed.mcp_url) {
                        self.copied = Some("MCP endpoint copied.");
                    }
                    ui.hyperlink_to("Connect from another device", REMOTE_GUIDE);
                    if let Some(message) = self.copied {
                        caption(ui, message);
                    }
                    ui.separator();
                    if self.confirm_rotation {
                        caption(ui, "Stop every server that shares this token file. Rotation revokes existing client credentials and signed links. Reconnect your agents after restarting the server.");
                        ui.horizontal_wrapped(|ui| {
                            if secondary(ui, "Cancel", true).clicked() {
                                self.confirm_rotation = false;
                            }
                            if primary(ui, "Confirm rotation", can_rotate).clicked() {
                                self.confirm_rotation = false;
                                rotate = true;
                            }
                        });
                    } else if secondary(ui, "Rotate token", can_rotate)
                        .on_disabled_hover_text("Stop Render Bridge before rotating its token.")
                        .clicked()
                    {
                        self.confirm_rotation = true;
                    }
                    if let Some(message) = &self.rotation_message {
                        caption(ui, message);
                    } else if self.worker.is_some() {
                        caption(ui, &self.progress);
                    }
                        });
                    secondary(ui, "Done", true).clicked()
                });
                if rotate {
                    self.rotate_token(ctx, installed);
                }
                if response.inner || response.should_close() {
                    self.panel_connect = false;
                }
            } else {
                self.panel_connect = false;
            }
        }
        #[cfg(windows)]
        if let Some(setting) = &mut self.startup_setting {
            let data_dir = self
                .controls
                .as_ref()
                .and_then(|controls| controls.data_dir.as_deref());
            setting.show(ctx, &mut self.appearance, data_dir, self.installed.as_ref());
        }
        #[cfg(windows)]
        {
            let blocked = if self.worker.is_some() {
                Some("Wait for the current operation to finish.")
            } else if self.running.as_mut().is_some_and(RunningBridge::is_running) {
                Some("Stop the server started by this panel before updating.")
            } else if self
                .controls
                .as_ref()
                .and_then(|controls| controls.queue.as_ref())
                .is_some_and(QueueView::has_active_work)
            {
                Some("Wait for running and queued jobs to finish before updating.")
            } else if self.installed.is_none() {
                Some("Open a saved Render Bridge installation to update.")
            } else {
                None
            };
            if self.update.show(ctx, self.installed.as_ref(), blocked) {
                if let Some(tray) = &mut self.tray_panel {
                    tray.exiting = true;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn actions(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("setup-actions")
            .exact_height(82.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .stroke(Stroke::new(1.0_f32, theme::color(ctx, LINE)))
                    .inner_margin(egui::Margin::symmetric(38, 17)),
            )
            .show(ctx, |ui| {
                let idle = self.worker.is_none();
                ui.horizontal(|ui| {
                    if let Some(cancel) = &self.install_cancel {
                        let cancellable = !cancel.load(Ordering::Relaxed);
                        if secondary(ui, "Cancel installation", cancellable).clicked() {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }
                    if self.step == Step::Connect && idle {
                        if self.running.is_some() {
                            if secondary(ui, "Stop Bridge", true).clicked() {
                                self.show_jobs(QueueAction::StopBridge);
                            }
                        } else if secondary(ui, "Close", true).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if self.running.is_none() && secondary(ui, "Change ComfyUI", true).clicked()
                        {
                            self.settings_changed();
                            if self.python_ready() {
                                self.change_step(Step::Comfy);
                            } else {
                                self.change_step(Step::Prerequisites);
                                self.check_python(ctx);
                            }
                        }
                    }
                    if matches!(self.step, Step::Comfy | Step::Models | Step::Install)
                        && secondary(ui, "Back", idle).clicked()
                    {
                        self.change_step(match self.step {
                            Step::Install => Step::Models,
                            Step::Models => Step::Comfy,
                            _ => Step::Prerequisites,
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (label, ready) = match self.step {
                            Step::Prerequisites if self.python_ready() => {
                                ("Continue to ComfyUI", idle)
                            }
                            Step::Prerequisites => ("Check Python again", idle),
                            Step::Comfy if self.can_continue() => ("Continue", idle),
                            Step::Comfy => (
                                "Check ComfyUI",
                                idle && !self.settings.comfy_root.trim().is_empty()
                                    && !self.settings.comfy_url.trim().is_empty(),
                            ),
                            Step::Models => ("Continue to install", self.can_continue() && idle),
                            Step::Install if self.install_error.is_some() => {
                                ("Retry installation", idle)
                            }
                            Step::Install => ("Install Render Bridge", idle),
                            Step::Connect if self.running.is_some() => ("Verify again", idle),
                            Step::Connect => ("Start & Verify", idle),
                        };
                        if primary(ui, label, ready).clicked() {
                            match self.step {
                                Step::Prerequisites if self.python_ready() => {
                                    self.change_step(Step::Comfy);
                                    if self.instances.is_empty() {
                                        self.start_discovery(ctx);
                                    }
                                }
                                Step::Prerequisites => self.check_python(ctx),
                                Step::Comfy if self.can_continue() => {
                                    self.change_step(Step::Models)
                                }
                                Step::Comfy => self.start_check(ctx),
                                Step::Models => self.change_step(Step::Install),
                                Step::Install => self.start_install(ctx),
                                Step::Connect => self.start_verification(ctx),
                            }
                        }
                    });
                });
            });
    }
}

impl eframe::App for Installer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_updates();
        if self.controls.is_some() {
            #[cfg(windows)]
            {
                if self
                    .tray_panel
                    .as_ref()
                    .is_some_and(|tray| !tray.exiting && !tray.icon.is_running())
                {
                    self.tray_panel = None;
                    self.tray_error = Some(
                        "Windows stopped the tray icon. You can keep reading the queue here."
                            .into(),
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
                let guard_exit = self.worker.is_some()
                    || self.update.blocks_controls()
                    || self.running.as_mut().is_some_and(RunningBridge::is_running);
                if self
                    .tray_panel
                    .as_mut()
                    .is_some_and(|tray| tray.update(ctx, guard_exit))
                {
                    return;
                }
                if let Some(action) = self
                    .tray_panel
                    .as_mut()
                    .and_then(|tray| tray.server_action.take())
                {
                    self.request_server_action(ctx, action);
                }
            }
            #[cfg(windows)]
            if self.tray_panel.is_none() && ctx.input(|input| input.viewport().close_requested()) {
                if self.worker.is_some() || self.update.blocks_controls() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                } else if self.running.as_mut().is_some_and(RunningBridge::is_running) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.show_jobs(QueueAction::CloseWindow);
                }
            }
            self.controls_screen(ctx);
            self.jobs_dialog(ctx);
            return;
        }
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if matches!(self.step, Step::Install | Step::Connect)
            && self.worker.is_some()
            && close_requested
        {
            if let Some(cancel) = &self.install_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if close_requested && self.running.as_mut().is_some_and(RunningBridge::is_running) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self
                .queue_dialog
                .as_ref()
                .is_none_or(|dialog| dialog.action != QueueAction::CloseWindow)
            {
                self.show_jobs(QueueAction::CloseWindow);
            }
        }
        self.rail(ctx);
        self.actions(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .inner_margin(egui::Margin::symmetric(38, 34)),
            )
            .show(ctx, |ui| {
                let step = self.step;
                egui::ScrollArea::vertical()
                    .id_salt(match step {
                        Step::Prerequisites => "prerequisites-page",
                        Step::Comfy => "comfy-page",
                        Step::Models => "models-page",
                        Step::Install => "install-page",
                        Step::Connect => "connect-page",
                    })
                    .auto_shrink([false, false])
                    .show(ui, |ui| match step {
                        Step::Prerequisites => self.prerequisites_screen(ui),
                        Step::Comfy => self.comfy_screen(ui),
                        Step::Models => self.models_screen(ui),
                        Step::Install => self.install_screen(ui),
                        Step::Connect => self.connect_screen(ui),
                    });
            });
        self.jobs_dialog(ctx);
    }
}

fn python_install_link() -> (&'static str, &'static str) {
    if cfg!(windows) {
        (
            "Get Python for Windows",
            "https://www.python.org/downloads/windows/",
        )
    } else if cfg!(target_os = "macos") {
        (
            "Get Python for macOS",
            "https://www.python.org/downloads/macos/",
        )
    } else {
        (
            "Python installation help",
            "https://docs.python.org/3/using/unix.html",
        )
    }
}

fn python_venv_help() -> &'static str {
    if cfg!(target_os = "linux") {
        "Install your distribution's virtual environment support for this Python. On Debian or Ubuntu, install python3-venv or the matching versioned package."
    } else {
        "Use the full Python installer from python.org, then choose Check Python again."
    }
}

fn bridge_status(code: &str) -> (&'static str, bool) {
    match code {
        "already_running" => ("Render Bridge is already running.", true),
        "ok" => (
            "Authenticated MCP connection verified. ComfyUI is reachable.",
            true,
        ),
        "comfyui_unavailable" => (
            "Authenticated MCP connection verified. Start the selected ComfyUI instance before rendering.",
            false,
        ),
        "token_unavailable" => (
            "The private Bridge token is unavailable. Check its file path and permissions.",
            false,
        ),
        "unauthorized" => (
            "The Bridge rejected authentication. Check the private token file and try again.",
            false,
        ),
        _ => (
            "Could not verify authenticated MCP. Check the Bridge and try again.",
            false,
        ),
    }
}

fn gaming_mode(installed: &Installed) -> Option<bool> {
    let flag = std::env::var_os("PAUSE_FLAG")
        .map(std::path::PathBuf::from)
        .map(|path| {
            if let Ok(rest) = path.strip_prefix("~")
                && let Some(home) = std::env::home_dir()
            {
                return home.join(rest);
            }
            installed.app_dir.join(path)
        })
        .unwrap_or_else(|| installed.data_dir.join("gaming_mode.flag"));
    flag.try_exists().ok()
}

fn model_status(availability: models::FileStatus, pipeline: bool) -> (&'static str, Color32) {
    use models::FileStatus::*;
    match (availability, pipeline) {
        (Found, true) => ("All files found", GREEN),
        (Missing, true) => ("Files missing", CAUTION),
        (Unknown, true) => ("Availability unconfirmed", MUTED),
        (Found, false) => ("Found", GREEN),
        (Missing, false) => ("Missing", CAUTION),
        (Unknown, false) => ("Unknown", MUTED),
    }
}

fn client_status(code: &str) -> (&'static str, bool) {
    match code {
        "config_matches" => (
            "Saved settings match. Open a fresh client session and call server_status.",
            true,
        ),
        "config_missing" | "server_missing" => (
            "No Bridge entry found. Follow the client setup instructions and verify again.",
            false,
        ),
        "config_unreadable" => (
            "Could not read these user settings. Check the path and file format.",
            false,
        ),
        "profile_missing" => ("The named Codex profile file was not found.", false),
        "server_disabled" => ("The Bridge entry is disabled in these settings.", false),
        "url_mismatch" => ("The saved URL differs from the MCP endpoint above.", false),
        "token_missing" | "token_mismatch" => (
            "Use the current token from the private token file in these client settings.",
            false,
        ),
        "auth_unverified" => (
            "These settings use dynamic authentication. Verify from a fresh client session.",
            false,
        ),
        "auth_conflict" | "config_conflict" => (
            "Several settings conflict. Choose one matching Bridge entry before verifying again.",
            false,
        ),
        _ => ("Could not verify these saved client settings.", false),
    }
}

fn header(ui: &mut Ui, title: &str, description: &str) {
    ui.add(egui::Label::new(RichText::new(title).size(36.0).strong()).wrap());
    ui.add(
        egui::Label::new(
            RichText::new(description)
                .size(17.0)
                .color(theme::color(ui.ctx(), MUTED)),
        )
        .wrap(),
    );
    ui.add_space(19.0);
}

fn bridge_mark(ui: &mut Ui) {
    let (bounds, _) = ui.allocate_exact_size(egui::vec2(47.0, 44.0), egui::Sense::hover());
    for (x, y, color) in [
        (2.0, 23.0, theme::color(ui.ctx(), MARK_SHADE)),
        (13.0, 13.0, theme::color(ui.ctx(), RAIL_ACTIVE)),
        (24.0, 3.0, theme::color(ui.ctx(), LIME)),
    ] {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(bounds.min + egui::vec2(x, y), egui::vec2(19.0, 19.0)),
            7,
            color,
        );
    }
}

fn card(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(theme::color(ui.ctx(), SURFACE))
        .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
        .corner_radius(16)
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}

fn panel_modal(ctx: &egui::Context, id: &'static str) -> egui::Modal {
    egui::Modal::new(egui::Id::new(id)).frame(
        Frame::popup(&ctx.style())
            .fill(theme::color(ctx, SURFACE))
            .stroke(Stroke::new(1.0_f32, theme::color(ctx, LINE)))
            .inner_margin(egui::Margin::same(12)),
    )
}

fn field_label(ui: &mut Ui, label: &str) -> egui::Id {
    ui.label(RichText::new(label).size(15.0).strong()).id
}

fn code_label(ui: &mut Ui, value: &str) {
    ui.add(
        egui::Label::new(RichText::new(value).monospace().size(13.0))
            .wrap()
            .selectable(true),
    );
}

fn text_edit(ui: &mut Ui, value: &mut String, width: f32, label: egui::Id) -> bool {
    ui.add(
        egui::TextEdit::singleline(value)
            .font(egui::TextStyle::Monospace)
            .margin(egui::Margin::symmetric(11, 12))
            .desired_width(width),
    )
    .labelled_by(label)
    .changed()
}

fn caption(ui: &mut Ui, message: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(message)
                .size(14.0)
                .color(theme::color(ui.ctx(), MUTED)),
        )
        .wrap(),
    );
}

fn status(ui: &mut Ui, title: &str, detail: &str, color: Color32) {
    ui.add_space(5.0);
    card(ui, |ui| {
        ui.label(
            RichText::new(title)
                .size(15.0)
                .strong()
                .color(theme::color(ui.ctx(), color)),
        );
        caption(ui, detail);
    });
}

fn activity(ui: &mut Ui, message: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::Spinner::new()
                .size(18.0)
                .color(theme::color(ui.ctx(), VIOLET)),
        );
        ui.label(
            RichText::new(message)
                .size(15.0)
                .color(theme::color(ui.ctx(), MUTED)),
        );
    });
}

fn primary(ui: &mut Ui, title: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            RichText::new(title)
                .size(15.0)
                .strong()
                .color(theme::color(ui.ctx(), INK)),
        )
        .fill(theme::color(ui.ctx(), LIME))
        .min_size(egui::vec2(143.0, 44.0))
        .corner_radius(12),
    )
}

fn secondary(ui: &mut Ui, title: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(title).size(14.0).strong())
            .min_size(egui::vec2(78.0, 44.0))
            .corner_radius(12),
    )
}

fn monitor_button(ui: &mut Ui, title: &str, enabled: bool, fill: Color32) -> egui::Response {
    let ctx = ui.ctx();
    let text = match fill {
        ERROR => CREAM,
        LINE => theme::color(ctx, MUTED),
        _ => theme::color(ctx, INK),
    };
    let fill = match (ctx.theme(), fill) {
        (egui::Theme::Dark, ERROR) => Color32::from_rgb(148, 53, 67),
        (egui::Theme::Dark, LINE) => Color32::from_rgb(28, 51, 49),
        _ => theme::color(ctx, fill),
    };
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(title).size(14.0).strong().color(text))
            .fill(fill)
            .min_size(egui::vec2(0.0, 34.0))
            .corner_radius(10),
    )
}

fn gaming_mode_button(ui: &mut Ui, on: bool, enabled: bool) -> egui::Response {
    let ctx = ui.ctx();
    let (label, fill, ink, border, hint) = if on {
        (
            "Gaming Mode · On",
            LIME,
            INK,
            LIME,
            "Allow agents to start GPU jobs again",
        )
    } else {
        (
            "Gaming Mode · Off",
            Color32::TRANSPARENT,
            theme::color(ctx, MUTED),
            theme::color(ctx, LINE).gamma_multiply(0.65),
            "Pause new GPU jobs from agents",
        )
    };
    let mut text = egui::text::LayoutJob::default();
    text.append(
        label,
        26.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(14.0),
            color: ink,
            ..Default::default()
        },
    );
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(11.0, 5.0);
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(1.0_f32, border);
            ui.add_enabled(
                enabled,
                egui::Button::new(text)
                    .fill(fill)
                    .min_size(egui::vec2(154.0, 30.0))
                    .corner_radius(10),
            )
        })
        .inner;
    if ui.is_rect_visible(response.rect) {
        let origin = response.rect.left_center() + egui::vec2(11.0, -6.0);
        let point = |x, y| origin + egui::vec2(x, y);
        let ink = if enabled {
            ink
        } else {
            ink.gamma_multiply(0.6)
        };
        let stroke = Stroke::new(1.35_f32, ink);
        let painter = ui.painter();
        let outline = [
            (5.0, 1.2),
            (13.0, 1.2),
            (15.6, 2.2),
            (17.1, 4.6),
            (18.0, 8.8),
            (17.6, 10.8),
            (16.2, 11.7),
            (14.9, 11.2),
            (12.1, 8.6),
            (5.9, 8.6),
            (3.1, 11.2),
            (1.8, 11.7),
            (0.4, 10.8),
            (0.0, 8.8),
            (0.9, 4.6),
            (2.4, 2.2),
        ];
        painter.add(egui::Shape::closed_line(
            outline.into_iter().map(|(x, y)| point(x, y)).collect(),
            stroke,
        ));
        painter.line_segment([point(3.7, 5.1), point(7.3, 5.1)], stroke);
        painter.line_segment([point(5.5, 3.3), point(5.5, 6.9)], stroke);
        painter.circle_filled(point(11.9, 4.5), 0.85, ink);
        painter.circle_filled(point(14.5, 6.3), 0.85, ink);
    }
    response.on_hover_text(hint)
}

fn model_paths(folder: &Path) -> String {
    let path = serde_json::Value::String(folder.to_string_lossy().into_owned());
    format!(
        "render_bridge_models:\n    base_path: {path}\n    checkpoints: checkpoints/\n    diffusion_models: diffusion_models/\n    text_encoders: text_encoders/\n    vae: vae/\n    loras: loras/\n"
    )
}

fn copy_agent_prompt(ui: &mut Ui, installed: &Installed) -> Option<&'static str> {
    if secondary(ui, "Copy agent setup prompt", true).clicked() {
        ui.ctx().copy_text(agent_setup_prompt(installed));
        Some("Prompt copied. Paste it into your agent on this computer.")
    } else {
        None
    }
}

fn agent_setup_prompt(installed: &Installed) -> String {
    format!(
        "Connect this MCP client to my installed Render Bridge server on this computer.\n\n\
         Server name: render_bridge\n\
         Transport: Streamable HTTP\n\
         MCP URL: {}\n\
         Private token file: {}\n\n\
         Use the active client's user settings and keep existing entries. Ask me first if the client requires permission to edit its configuration. If an entry already uses this name, check it before changing it. Use the private token file for bearer authentication, preferably with the client's secret storage or a private environment variable. Keep the token out of chat, logs, command-line arguments, and project files.\n\n\
         If the server is offline or the token file is missing, ask me to click Start server in the Jobs panel or Start & Verify in Setup. After configuring it, initialize the connection, list its tools, and call server_status before submitting any jobs. If MCP settings load only at startup, tell me to start a fresh session and verify there.\n\n\
         Connection guide: {CLIENT_GUIDE}\n\
         Agent guide: https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/AGENTS.md",
        installed.mcp_url,
        installed.token_path.display(),
    )
}

fn copy_row(ui: &mut Ui, title: &str, button: &str, value: &str) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        field_label(ui, title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if secondary(ui, button, true).clicked() {
                ui.ctx().copy_text(value.to_owned());
                clicked = true;
            }
        });
    });
    code_label(ui, value);
    clicked
}

fn launch_mode(mut args: impl Iterator<Item = std::ffi::OsString>) -> LaunchMode {
    if !args.next().is_some_and(|value| value == "--job-monitor") {
        return LaunchMode::Setup;
    }
    let flag = args.next();
    let value = args.next();
    let option = args.next();
    let start_hidden = option
        .as_deref()
        .is_some_and(|value| value == "--start-in-tray");
    let valid_options = (option.is_none() || start_hidden) && args.next().is_none();
    let target = match (
        valid_options,
        flag.as_deref().and_then(std::ffi::OsStr::to_str),
        value,
    ) {
        (true, Some("--data-dir"), Some(value)) if Path::new(&value).is_absolute() => {
            MonitorTarget::Installation(PathBuf::from(value))
        }
        (true, Some("--comfy-url"), Some(value)) => value
            .to_str()
            .and_then(|url| setup::validate_comfy_url(url).ok())
            .map_or(MonitorTarget::Missing, MonitorTarget::ComfyUrl),
        _ => MonitorTarget::Missing,
    };
    LaunchMode::Controls {
        start_hidden: start_hidden && !matches!(&target, MonitorTarget::Missing),
        target,
    }
}

#[cfg(windows)]
fn tray_key(mode: &LaunchMode) -> Option<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hash = DefaultHasher::new();
    match mode {
        LaunchMode::Controls {
            target: MonitorTarget::ComfyUrl(url),
            ..
        } => ("url", url).hash(&mut hash),
        LaunchMode::Controls {
            target: MonitorTarget::Installation(directory),
            ..
        } => ("installation", directory).hash(&mut hash),
        _ => return None,
    }
    Some(format!("{:016x}", hash.finish()))
}

#[cfg(windows)]
fn startup_setting(mode: &LaunchMode) -> Option<StartupSetting> {
    use std::ffi::OsString;
    let (flag, value) = match mode {
        LaunchMode::Controls {
            target: MonitorTarget::ComfyUrl(url),
            ..
        } => ("--comfy-url", OsString::from(url)),
        LaunchMode::Controls {
            target: MonitorTarget::Installation(directory),
            ..
        } => ("--data-dir", directory.as_os_str().to_owned()),
        _ => return None,
    };
    Some(StartupSetting {
        executable: std::env::current_exe().ok()?,
        arguments: vec![
            OsString::from("--job-monitor"),
            OsString::from(flag),
            value,
            OsString::from("--start-in-tray"),
        ],
        open: false,
        enabled: None,
        error: None,
        appearance_error: None,
        assets: asset_ui::AssetEditor::default(),
    })
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn shortcut_opens_controls_for_the_exact_data_directory() {
        let data = std::env::current_dir()
            .unwrap()
            .join("Render Bridge fixture");
        let arguments = [
            OsString::from("--job-monitor"),
            OsString::from("--data-dir"),
            data.clone().into_os_string(),
        ];
        assert!(matches!(
            launch_mode(arguments.into_iter()),
            LaunchMode::Controls { target: MonitorTarget::Installation(directory), start_hidden: false, .. } if directory == data
        ));
        assert!(matches!(
            launch_mode([OsString::from("--job-monitor")].into_iter()),
            LaunchMode::Controls {
                target: MonitorTarget::Missing,
                start_hidden: false,
                ..
            }
        ));
    }

    #[test]
    fn standalone_queue_monitor_requires_a_loopback_origin() {
        let launch = |url: &str| {
            launch_mode(
                [
                    OsString::from("--job-monitor"),
                    OsString::from("--comfy-url"),
                    OsString::from(url),
                ]
                .into_iter(),
            )
        };
        assert!(matches!(launch("http://127.0.0.1:8188/"),
            LaunchMode::Controls { target: MonitorTarget::ComfyUrl(url), .. } if url == "http://127.0.0.1:8188"));
        assert!(matches!(
            launch("http://example.test:8188"),
            LaunchMode::Controls {
                target: MonitorTarget::Missing,
                start_hidden: false,
                ..
            }
        ));
    }

    #[test]
    fn hidden_start_requires_a_valid_monitor_target() {
        let mode = |url: &str| {
            launch_mode(
                ["--job-monitor", "--comfy-url", url, "--start-in-tray"]
                    .into_iter()
                    .map(OsString::from),
            )
        };
        assert!(matches!(
            mode("http://127.0.0.1:8188"),
            LaunchMode::Controls {
                start_hidden: true,
                ..
            }
        ));
        assert!(matches!(
            mode("https://example.test"),
            LaunchMode::Controls {
                start_hidden: false,
                ..
            }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn startup_preserves_the_selected_installation() {
        let folder = std::env::current_dir()
            .unwrap()
            .join("Render Bridge fixture");
        let arguments = vec![
            OsString::from("--job-monitor"),
            OsString::from("--data-dir"),
            folder.clone().into_os_string(),
            OsString::from("--start-in-tray"),
        ];
        let mode = launch_mode(arguments.clone().into_iter());
        assert!(matches!(
            &mode,
            LaunchMode::Controls { target: MonitorTarget::Installation(path), start_hidden: true } if path == &folder
        ));
        assert_eq!(startup_setting(&mode).unwrap().arguments, arguments);
    }
}

fn main() -> std::process::ExitCode {
    diagnostics::install_panic_hook();
    if open_app().is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        diagnostics::window_error();
        std::process::ExitCode::FAILURE
    }
}

fn open_app() -> eframe::Result {
    let args = std::env::args_os().skip(1);
    #[cfg(windows)]
    let mode = {
        let mut args = args.peekable();
        match args.peek().and_then(|value| value.to_str()) {
            Some("--apply-update") => {
                args.next();
                return update_ui::apply(update_handoff::UpdateRequest::parse(args));
            }
            Some("--finish-update") => {
                args.next();
                let request = match update_handoff::FinishRequest::parse(args) {
                    Ok(request) => request,
                    Err(error) => return update_ui::apply(Err(error)),
                };
                let _ = request.cleanup_after_helper();
                LaunchMode::Controls {
                    target: MonitorTarget::Installation(request.data_dir),
                    start_hidden: false,
                }
            }
            _ => launch_mode(args),
        }
    };
    #[cfg(not(windows))]
    let mode = launch_mode(args);
    let (title, size, minimum, start_hidden) = match &mode {
        LaunchMode::Setup => (
            "Render Bridge Installer",
            Vec2::new(990.0, 670.0),
            Vec2::new(760.0, 540.0),
            false,
        ),
        LaunchMode::Controls { start_hidden, .. } => (
            "Render Bridge",
            Vec2::new(480.0, 365.0),
            Vec2::new(408.0, 330.0),
            cfg!(windows) && *start_hidden,
        ),
    };
    #[cfg(windows)]
    let key = tray_key(&mode);
    #[cfg(windows)]
    let startup_setting = startup_setting(&mode);
    #[cfg(windows)]
    if key
        .as_deref()
        .is_some_and(|key| tray::activate_existing(key, !start_hidden))
    {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icons::window_icon())
            .with_inner_size(size)
            .with_min_inner_size(minimum)
            .with_visible(!start_hidden)
            .with_active(!start_hidden)
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(move |creation| {
            let installer = Installer::new(&creation.egui_ctx, mode);
            #[cfg(windows)]
            let installer = {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                let mut installer = installer;
                installer.startup_setting = startup_setting;
                if let Some(key) = key {
                    let server_controls = installer
                        .controls
                        .as_ref()
                        .is_some_and(|controls| controls.data_dir.is_some());
                    let root =
                        creation
                            .window_handle()
                            .ok()
                            .and_then(|window| match window.as_raw() {
                                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                                _ => None,
                            });
                    let tray = root
                        .ok_or_else(|| {
                            "Windows did not provide the Render Bridge window handle.".to_owned()
                        })
                        .and_then(|window| {
                            tray::Tray::start(
                                creation.egui_ctx.clone(),
                                &key,
                                window,
                                server_controls,
                            )
                        });
                    match tray {
                        Ok(icon) => {
                            installer.tray_panel = Some(TrayPanel {
                                icon,
                                hidden: start_hidden,
                                hidden_start_frames: if start_hidden { 2 } else { 0 },
                                exiting: false,
                                server_action: None,
                            });
                        }
                        Err(message) => {
                            installer.tray_error = Some(message);
                            creation
                                .egui_ctx
                                .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        }
                    }
                }
                installer
            };
            Ok(Box::new(installer))
        }),
    )
}

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod artwork;
mod asset_folders;
mod asset_ui;
mod connection;
mod diagnostics;
mod discovery;
mod first_render;
mod icons;
#[cfg(windows)]
mod job_progress;
mod jobs;
mod jobs_panel;
mod model_download;
mod model_library;
mod model_ui;
mod models;
mod node_packages;
mod node_view;
mod pipeline;
mod pipeline_ui;
#[cfg(windows)]
mod python_install;
mod setup;
mod shortcuts;
#[cfg(windows)]
mod startup;
mod theme;
#[cfg(windows)]
mod theme_ui;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod uninstall;
#[cfg(windows)]
mod uninstall_ui;
#[cfg(windows)]
mod update_handoff;
#[cfg(windows)]
mod update_ui;
#[cfg(windows)]
mod updates;
#[cfg(windows)]
mod utility_ui;
mod workflow_editor;
mod workflow_ui;

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

macro_rules! guide_url {
    ($path:literal) => {
        concat!(
            "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/",
            env!("RENDER_BRIDGE_DOCS_REF"),
            "/",
            $path
        )
    };
}

const COMFY_GUIDE: &str = "https://docs.comfy.org/installation/manual_install";
const COMFY_DOWNLOAD: &str = "https://comfy.org/download";
const CLAUDE_SETUP: &str = "https://code.claude.com/docs/en/quickstart";
const CODEX_SETUP: &str = "https://openai.com/codex/get-started/";
const MODEL_GUIDE: &str = guide_url!("docs/pipelines.md");
const CLIENT_GUIDE: &str = guide_url!("README.md#connect-an-mcp-client");
const AGENT_GUIDE: &str = guide_url!("AGENTS.md");
const PRIVACY_GUIDE: &str = guide_url!("PRIVACY.md");
const REMOTE_GUIDE: &str = guide_url!("docs/connections.md#connect-from-another-device");
#[cfg(windows)]
const SETTINGS_GUIDE: &str = guide_url!("docs/configuration.md");
#[cfg(windows)]
const RELEASE_NOTES: &str = guide_url!("CHANGELOG.md");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Prerequisites,
    Comfy,
    Models,
    Install,
    Connect,
}

const STEPS: [(Step, &str); 5] = [
    (Step::Prerequisites, "Prerequisites"),
    (Step::Comfy, "ComfyUI"),
    (Step::Models, "Models"),
    (Step::Install, "Install"),
    (Step::Connect, "Connect"),
];

enum Event {
    PythonChecked(PythonReadiness),
    #[cfg(windows)]
    PythonInstallFinished(Result<PythonReadiness, String>),
    Checked(CheckOutcome),
    Discovered(Vec<discovery::Instance>),
    Verified(
        VerificationTarget,
        Option<RunningBridge>,
        Result<serde_json::Value, String>,
    ),
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

#[derive(Default)]
struct SetupLaunch {
    config: Option<PathBuf>,
    directory: Option<PathBuf>,
    invalid: bool,
}

enum LaunchMode {
    Setup(SetupLaunch),
    Controls {
        target: MonitorTarget,
        start_hidden: bool,
        start_server: bool,
    },
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum ServerAction {
    Start,
    StartSilently,
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
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    Appearance,
    Storage,
    Advanced,
    Diagnostics,
}

#[cfg(windows)]
impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Storage,
        Self::Advanced,
        Self::Diagnostics,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Storage => "Storage",
            Self::Advanced => "Advanced",
            Self::Diagnostics => "Diagnostics",
        }
    }
}

#[cfg(windows)]
struct StartupSetting {
    executable: PathBuf,
    arguments: Vec<std::ffi::OsString>,
    comfy_url: Option<String>,
    open: bool,
    selected: SettingsTab,
    enabled: Option<bool>,
    error: Option<String>,
    appearance_error: Option<String>,
    message: Option<Result<&'static str, String>>,
    theme_editor: theme_ui::ThemeEditor,
    assets: asset_ui::AssetEditor,
    model_destination: Option<PathBuf>,
    model_destination_changed: bool,
    diagnostics: SettingsDiagnostics,
}

#[cfg(windows)]
impl StartupSetting {
    fn open(&mut self, ctx: &egui::Context, installed: Option<&Installed>) {
        if self.open {
            ctx.send_viewport_cmd_to(settings_viewport_id(), egui::ViewportCommand::Focus);
            return;
        }
        self.open = true;
        self.selected = SettingsTab::default();
        self.message = None;
        self.diagnostics = SettingsDiagnostics::default();
        self.model_destination =
            installed.and_then(|installed| model_library::load_destination(&installed.data_dir));
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
        let size = settings_window_size(ctx.input(|input| input.viewport().monitor_size));
        let closed = ctx.show_viewport_immediate(
            settings_viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("Render Bridge Settings")
                .with_icon(icons::window_icon())
                .with_inner_size(size)
                .with_min_inner_size(size.min(egui::vec2(340.0, 270.0)))
                .with_clamp_size_to_monitor_size(true),
            |ctx, class| {
                if class == egui::ViewportClass::Embedded {
                    let response = panel_modal(ctx, "render-bridge-settings").show(ctx, |ui| {
                        let viewport = ctx.content_rect();
                        ui.set_width((viewport.width() - 54.0).clamp(120.0, 500.0));
                        ui.heading("Settings");
                        self.navigation(ui, false);
                        egui::ScrollArea::vertical()
                            .id_salt(self.selected.title())
                            .max_height((viewport.height() - 178.0).max(40.0))
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.contents(ui, appearance, data_dir, installed));
                        ui.separator();
                        Self::actions(ui)
                    });
                    return response.inner || response.should_close();
                }

                let fill = theme::color(ctx, RAIL);
                egui::TopBottomPanel::top("settings-title")
                    .frame(
                        Frame::new()
                            .fill(fill)
                            .inner_margin(egui::Margin::symmetric(14, 4)),
                    )
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            bridge_mark(ui);
                            ui.label(
                                RichText::new("Render Bridge")
                                    .size(17.0)
                                    .strong()
                                    .color(theme::on_fill(fill)),
                            );
                        });
                    });
                let surface = theme::color(ctx, SURFACE);
                let roomy = ctx.content_rect().width() >= SETTINGS_RAIL_MIN_WIDTH;
                if roomy {
                    egui::SidePanel::left("settings-navigation")
                        .exact_width(160.0)
                        .resizable(false)
                        .frame(
                            Frame::new()
                                .fill(fill)
                                .stroke(Stroke::new(1.0_f32, theme::color(ctx, LINE)))
                                .inner_margin(egui::Margin::symmetric(11, 15)),
                        )
                        .show(ctx, |ui| {
                            ui.label(theme::strong("Settings", 22.0).color(theme::on_fill(fill)));
                            ui.add_space(7.0);
                            self.navigation(ui, true);
                        });
                } else {
                    egui::TopBottomPanel::top("settings-navigation")
                        .frame(
                            Frame::new()
                                .fill(surface)
                                .inner_margin(egui::Margin::symmetric(8, 5)),
                        )
                        .show(ctx, |ui| self.navigation(ui, false));
                }
                let done = egui::TopBottomPanel::bottom("settings-actions")
                    .frame(
                        Frame::new()
                            .fill(surface)
                            .inner_margin(egui::Margin::symmetric(14, 8)),
                    )
                    .show(ctx, Self::actions)
                    .inner;
                egui::CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(theme::color(ctx, CREAM))
                            .inner_margin(egui::Margin::same(14)),
                    )
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(self.selected.title())
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.contents(ui, appearance, data_dir, installed));
                    });
                done || ctx.input(|input| {
                    input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
                })
            },
        );
        if closed {
            self.open = false;
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }

    fn navigation(&mut self, ui: &mut Ui, rail: bool) {
        let labels = |ui: &mut Ui| {
            for tab in SettingsTab::ALL {
                let width = if rail { ui.available_width() } else { 0.0 };
                let choice = if rail {
                    settings_rail_choice(ui, tab, self.selected == tab, width)
                } else {
                    settings_choice(ui, tab.title(), self.selected == tab, width)
                };
                if choice.clicked() && self.selected != tab {
                    self.selected = tab;
                    self.message = None;
                }
            }
        };
        if rail {
            ui.vertical(labels);
        } else {
            ui.horizontal_wrapped(labels);
        }
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        appearance: &mut egui::ThemePreference,
        data_dir: Option<&Path>,
        installed: Option<&Installed>,
    ) {
        theme::heading(ui, self.selected.title(), 31.0);
        ui.add_space(8.0);
        match self.selected {
            SettingsTab::Appearance => self.appearance(ui, appearance, data_dir),
            SettingsTab::Storage => self.storage(ui, installed),
            SettingsTab::Advanced => self.advanced(ui, installed),
            SettingsTab::Diagnostics => self.diagnostics(ui, installed),
        }
        if let Some(message) = &self.message {
            match message {
                Ok(message) => status(ui, "Saved", message, GREEN),
                Err(error) => status(ui, "Action unavailable", error, CAUTION),
            }
        }
    }

    fn appearance(
        &mut self,
        ui: &mut Ui,
        appearance: &mut egui::ThemePreference,
        data_dir: Option<&Path>,
    ) {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                field_label(ui, "Theme");
                for (label, preference) in [
                    ("Dark", egui::ThemePreference::Dark),
                    ("Light", egui::ThemePreference::Light),
                    ("System", egui::ThemePreference::System),
                ] {
                    if settings_choice(ui, label, *appearance == preference, 70.0).clicked()
                        && *appearance != preference
                    {
                        match theme::save(data_dir, preference) {
                            Ok(()) => {
                                *appearance = preference;
                                self.appearance_error = None;
                                ui.ctx().set_theme(preference);
                                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                            }
                            Err(error) => self.appearance_error = Some(error),
                        }
                    }
                }
            });
            if let Some(error) = &self.appearance_error {
                caption(ui, error);
            }
            ui.add_space(8.0);
            theme_preview(ui);
        });
        card(ui, |ui| {
            let mut enabled = self.enabled.unwrap_or_default();
            if ui
                .add_enabled(
                    self.enabled.is_some(),
                    egui::Checkbox::new(&mut enabled, "Start hidden when I sign in"),
                )
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
                caption(ui, error);
            }
        });
        setup_card(ui, |ui| self.theme_editor.show(ui, data_dir));
    }

    fn storage(&mut self, ui: &mut Ui, installed: Option<&Installed>) {
        let Some(installed) = installed else {
            caption(
                ui,
                "Open a saved Render Bridge installation to change its folders.",
            );
            return;
        };

        card(ui, |ui| {
            let previous = self.model_destination.clone();
            if asset_ui::compact_folder_option(
                ui,
                "Model storage folder",
                &mut self.model_destination,
                "Automatic. Use folders registered by the active ComfyUI build.",
            ) {
                self.message = Some(
                    match model_library::save_destination(
                        &installed.data_dir,
                        self.model_destination.as_deref(),
                    ) {
                        Ok(()) => {
                            self.model_destination_changed = true;
                            Ok("Model storage updated.")
                        }
                        Err(error) => {
                            self.model_destination = previous;
                            Err(error)
                        }
                    },
                );
            }
            ui.separator();
            match setup::saved_install_settings(installed) {
                Ok(settings) => self.assets.settings(
                    ui,
                    &setup::folder(&settings, "output"),
                    &setup::folder(&settings, "input"),
                    &installed.data_dir,
                ),
                Err(error) => caption(ui, &error),
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                field_label(ui, "Hugging Face token");
                caption(
                    ui,
                    if std::env::var_os("HF_TOKEN").is_some_and(|value| !value.is_empty()) {
                        "Available from the environment."
                    } else {
                        "Optional for gated downloads."
                    },
                );
            });
        });
    }

    fn advanced(&mut self, ui: &mut Ui, installed: Option<&Installed>) {
        let Some(installed) = installed else {
            if let Some(url) = &self.comfy_url {
                card(ui, |ui| {
                    field_label(ui, "ComfyUI address");
                    code_label(ui, url);
                });
            }
            caption(
                ui,
                "Open a saved Render Bridge installation to edit or export its configuration.",
            );
            return;
        };

        card(ui, |ui| {
            field_label(ui, "Installation folder");
            code_label(ui, &installed.data_dir.to_string_lossy());
            ui.separator();
            match setup::saved_install_settings(installed) {
                Ok(settings) => {
                    field_label(ui, "ComfyUI address");
                    code_label(ui, &settings.comfy_url);
                    ui.collapsing("Input and output folders", |ui| {
                        for (label, kind) in
                            [("Input folder", "input"), ("Output folder", "output")]
                        {
                            field_label(ui, label);
                            code_label(ui, &setup::folder(&settings, kind).to_string_lossy());
                        }
                    });
                }
                Err(error) => caption(ui, &error),
            }
        });

        card(ui, |ui| {
            field_label(ui, "Configuration file");
            caption(
                ui,
                "Stop Render Bridge before editing its active configuration. Start it again to verify your changes.",
            );
            ui.horizontal_wrapped(|ui| {
                if secondary(ui, "Open configuration file", true).clicked() {
                    self.message = open_configuration_file(installed).err().map(Err);
                }
                if secondary(ui, "Export setup config", true).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Render Bridge settings", &["json"])
                        .set_file_name("settings.json")
                        .save_file()
                {
                    self.message = Some(
                        setup::export_setup_config(installed, &path)
                            .map(|()| "Setup config exported."),
                    );
                }
            });
            ui.hyperlink_to("Server and model settings", SETTINGS_GUIDE);
        });
    }

    fn diagnostics(&mut self, ui: &mut Ui, installed: Option<&Installed>) {
        card(ui, |ui| {
            self.diagnostics
                .show(ui, installed, self.comfy_url.as_deref())
        });
        card(ui, |ui| {
            field_label(ui, "Crash logs");
            let logs = diagnostics::existing_directory();
            if secondary(ui, "Open crash logs", logs.is_some())
                .on_disabled_hover_text("No local crash logs yet.")
                .clicked()
                && let Some(folder) = logs
                && !diagnostics::open_directory(&folder)
            {
                self.message = Some(Err("Could not open the crash logs folder.".into()));
            }
        });
        ui.horizontal_wrapped(|ui| {
            caption(ui, concat!("Version ", env!("CARGO_PKG_VERSION")));
            ui.hyperlink_to("Release notes", RELEASE_NOTES);
        });
    }

    fn take_model_destination_change(&mut self) -> bool {
        std::mem::take(&mut self.model_destination_changed)
    }

    fn actions(ui: &mut Ui) -> bool {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            settings_choice(ui, "Done", true, 72.0).clicked()
        })
        .inner
    }
}

#[cfg(windows)]
#[derive(Default)]
struct SettingsDiagnostics {
    target: Option<(Option<PathBuf>, Option<String>)>,
    worker: Option<Receiver<SettingsHealth>>,
    result: Option<SettingsHealth>,
    error: Option<&'static str>,
}

#[cfg(windows)]
struct SettingsHealth {
    bridge_listening: Option<bool>,
    queue: Option<Result<jobs::QueueStatus, String>>,
}

#[cfg(windows)]
impl SettingsDiagnostics {
    fn show(&mut self, ui: &mut Ui, installed: Option<&Installed>, standalone_url: Option<&str>) {
        let url = installed
            .and_then(setup::installed_comfy_url)
            .or_else(|| standalone_url.map(str::to_owned));
        let target = (
            installed.map(|installed| installed.data_dir.clone()),
            url.clone(),
        );
        if self.target.as_ref() != Some(&target) {
            self.target = Some(target);
            self.result = None;
            self.start(ui.ctx(), installed.cloned(), url.clone());
        }
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(result) => {
                    self.result = Some(result);
                    self.worker = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("The status check did not finish.");
                    self.worker = None;
                }
            }
        }

        field_label(ui, "Service status");
        let bridge_listening = self
            .result
            .as_ref()
            .and_then(|result| result.bridge_listening);
        let queue = self
            .result
            .as_ref()
            .and_then(|result| result.queue.as_ref());
        let bridge = match (installed.is_some(), bridge_listening) {
            (_, Some(true)) => ("Listening", GREEN),
            (_, Some(false)) => ("Unverified", CAUTION),
            (true, None) => ("Waiting for check", MUTED),
            (false, None) => ("No saved installation", MUTED),
        };
        let comfy = match (url.is_some(), queue) {
            (_, Some(Ok(_))) => ("Responding", GREEN),
            (_, Some(Err(_))) => ("Unverified", CAUTION),
            (true, None) => ("Waiting for check", MUTED),
            (false, None) => ("No backend selected", MUTED),
        };
        Self::row(ui, "Render Bridge", bridge.0, bridge.1);
        ui.separator();
        Self::row(ui, "ComfyUI", comfy.0, comfy.1);
        if let Some(Ok(queue)) = queue {
            ui.separator();
            Self::row(
                ui,
                "Queue",
                &format!("{} running, {} queued", queue.running, queue.queued),
                MUTED,
            );
        }
        if let Some(Err(error)) = queue {
            caption(ui, error);
        }
        if let Some(error) = self.error {
            caption(ui, error);
        }
        if secondary(
            ui,
            if self.worker.is_some() {
                "Checking…"
            } else {
                "Recheck services"
            },
            self.worker.is_none() && (installed.is_some() || url.is_some()),
        )
        .clicked()
        {
            self.start(ui.ctx(), installed.cloned(), url);
        }
    }

    fn row(ui: &mut Ui, label: &str, state: &str, color: Color32) {
        let state = RichText::new(state).color(theme::color(ui.ctx(), color));
        if ui.available_width() >= 300.0 {
            ui.horizontal(|ui| {
                ui.label(theme::strong(label, 15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(state)
                });
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::strong(label, 15.0));
                ui.label(state);
            });
        }
    }

    fn start(&mut self, ctx: &egui::Context, installed: Option<Installed>, url: Option<String>) {
        self.worker = None;
        self.error = None;
        if installed.is_none() && url.is_none() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let viewport = ctx.viewport_id();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("render-bridge-diagnostics".into())
            .spawn(move || {
                let bridge_listening = installed.as_ref().map(connection::bridge_is_listening);
                let queue = url.as_deref().map(jobs::check);
                let _ = send.send(SettingsHealth {
                    bridge_listening,
                    queue,
                });
                ctx.request_repaint_of(viewport);
            }) {
            Ok(_) => self.worker = Some(receive),
            Err(_) => self.error = Some("The status check could not start."),
        }
    }
}

#[cfg(windows)]
fn settings_choice(ui: &mut Ui, label: &str, selected: bool, width: f32) -> egui::Response {
    let fill = theme::color(ui.ctx(), if selected { LIME } else { SURFACE });
    let text = if selected {
        theme::on_fill(fill)
    } else {
        theme::color(ui.ctx(), INK)
    };
    ui.add(
        egui::Button::new(RichText::new(label).color(text).strong())
            .selected(selected)
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
            .corner_radius(8)
            .min_size(egui::vec2(width, 34.0)),
    )
}

#[cfg(windows)]
fn settings_rail_choice(
    ui: &mut Ui,
    tab: SettingsTab,
    selected: bool,
    width: f32,
) -> egui::Response {
    let (bounds, response) = ui.allocate_exact_size(egui::vec2(width, 39.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, tab.title())
    });
    let fill = theme::color(
        ui.ctx(),
        if selected {
            LIME
        } else if response.hovered() {
            RAIL_ACTIVE
        } else {
            RAIL
        },
    );
    let ink = theme::on_fill(fill);
    let focus = if response.has_focus() {
        Stroke::new(2.0_f32, theme::color(ui.ctx(), VIOLET))
    } else {
        Stroke::NONE
    };
    let painter = ui.painter_at(bounds);
    painter.rect(bounds, 8, fill, focus, egui::StrokeKind::Inside);
    painter.text(
        egui::pos2(bounds.left() + 31.0, bounds.center().y),
        egui::Align2::LEFT_CENTER,
        tab.title(),
        theme::display_font(14.0),
        ink,
    );
    let center = egui::pos2(bounds.left() + 15.0, bounds.center().y);
    let stroke = Stroke::new(1.4_f32, ink);
    let at = |x, y| center + egui::vec2(x, y);
    match tab {
        SettingsTab::Appearance => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(-6.0, -5.0), at(6.0, 3.0)),
                1,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([at(0.0, 3.0), at(0.0, 6.0)], stroke);
            painter.line_segment([at(-4.0, 6.0), at(4.0, 6.0)], stroke);
        }
        SettingsTab::Storage => {
            for y in [-4.0, 0.0, 4.0] {
                painter.rect_stroke(
                    egui::Rect::from_min_max(at(-6.0, y - 2.0), at(6.0, y + 2.0)),
                    2,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        SettingsTab::Advanced => {
            for (x, y) in [(-3.0, -4.0), (3.0, 4.0)] {
                painter.line_segment([at(-6.0, y), at(6.0, y)], stroke);
                painter.circle_filled(at(x, y), 2.1, ink);
            }
        }
        SettingsTab::Diagnostics => {
            painter.add(egui::Shape::line(
                vec![
                    at(-7.0, 0.0),
                    at(-4.0, 0.0),
                    at(-1.0, -5.0),
                    at(2.0, 5.0),
                    at(4.0, 0.0),
                    at(7.0, 0.0),
                ],
                stroke,
            ));
        }
    }
    response
}

#[cfg(windows)]
fn theme_preview(ui: &mut Ui) {
    let width = ui.available_width().min(346.0);
    if width < 140.0 {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - width) * 0.5).max(0.0));
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 124.0), egui::Sense::hover());
        let fill = |role| theme::color(ui.ctx(), role);
        let painter = ui.painter();
        painter.rect(
            rect,
            11,
            fill(CREAM),
            Stroke::new(1.0_f32, fill(LINE)),
            egui::StrokeKind::Inside,
        );
        let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 24.0));
        painter.rect_filled(
            top,
            egui::CornerRadius {
                nw: 11,
                ne: 11,
                ..Default::default()
            },
            fill(RAIL),
        );
        for (x, y, role) in [
            (9.0, 11.0, MARK_SHADE),
            (14.0, 7.0, RAIL_ACTIVE),
            (19.0, 3.0, LIME),
        ] {
            painter.rect_filled(
                egui::Rect::from_min_size(top.min + egui::vec2(x, y), egui::vec2(8.0, 8.0)),
                2,
                fill(role),
            );
        }
        for offset in [36.0, 50.0, 64.0] {
            let x = top.max.x - offset;
            painter.line_segment(
                [
                    egui::pos2(x, top.center().y),
                    egui::pos2(x + 5.0, top.center().y),
                ],
                Stroke::new(1.0_f32, theme::on_fill(fill(RAIL))),
            );
        }
        let rail = egui::Rect::from_min_max(
            rect.min + egui::vec2(9.0, 31.0),
            egui::pos2(rect.min.x + width * 0.33, rect.max.y - 8.0),
        );
        painter.rect_filled(rail, 7, fill(RAIL));
        let rail_row = |offset, ratio| {
            egui::Rect::from_min_size(
                rail.min + egui::vec2(6.0, offset),
                egui::vec2((rail.width() - 12.0) * ratio, 10.0),
            )
        };
        painter.rect_filled(rail_row(6.0, 1.0), 3, fill(LIME));
        for (offset, ratio) in [(25.0, 0.72), (43.0, 0.86), (61.0, 0.65)] {
            painter.rect_filled(rail_row(offset, ratio), 3, fill(LINE));
        }
        let pane = egui::Rect::from_min_max(
            egui::pos2(rail.max.x + 8.0, rail.min.y),
            rect.max - egui::vec2(10.0, 8.0),
        );
        painter.rect_filled(pane, 7, fill(SURFACE));
        for (offset, ratio) in [(11.0, 0.49), (26.0, 0.76), (67.0, 0.58)] {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    pane.min + egui::vec2(9.0, offset),
                    egui::vec2(pane.width() * ratio, 5.0),
                ),
                2,
                fill(LINE),
            );
        }
        let gutter = 8.0;
        let card_width = (pane.width() - 18.0 - gutter) * 0.5;
        for offset in [9.0, 9.0 + card_width + gutter] {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    pane.min + egui::vec2(offset, 40.0),
                    egui::vec2(card_width, 18.0),
                ),
                4,
                fill(RAIL_ACTIVE),
            );
        }
        response.on_hover_text("Preview of the active theme");
    });
}

#[cfg(windows)]
const SETTINGS_RAIL_MIN_WIDTH: f32 = 520.0;

#[cfg(windows)]
fn settings_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("render-bridge-settings")
}

#[cfg(windows)]
fn settings_window_size(monitor: Option<Vec2>) -> Vec2 {
    let preferred = egui::vec2(660.0, 560.0);
    monitor.map_or(preferred, |size| {
        // Leave room for Windows decorations and the taskbar at the monitor's scale.
        preferred.min((size - egui::vec2(32.0, 64.0)).max(egui::vec2(1.0, 1.0)))
    })
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
                tray::TrayEvent::StartSilently if !self.exiting && self.server_action.is_none() => {
                    self.server_action = Some(ServerAction::StartSilently);
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

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
enum WorkspaceTab {
    #[default]
    Jobs,
    Models,
    Pipelines,
    Nodes,
    Connect,
}

const WORKSPACE_TABS: [(WorkspaceTab, &str); 5] = [
    (WorkspaceTab::Jobs, "Jobs"),
    (WorkspaceTab::Models, "Models"),
    (WorkspaceTab::Pipelines, "Pipelines"),
    (WorkspaceTab::Nodes, "Nodes"),
    (WorkspaceTab::Connect, "Connect"),
];

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ConnectTarget {
    #[default]
    Local,
    Network,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ConnectClient {
    #[default]
    Codex,
    Claude,
    Other,
}

impl ConnectClient {
    fn verifier(self) -> Option<Client> {
        match self {
            Self::Codex => Some(Client::Codex),
            Self::Claude => Some(Client::Claude),
            Self::Other => None,
        }
    }

    fn label(self) -> &'static str {
        self.verifier().map_or("Other clients", Client::label)
    }

    fn guide(self) -> &'static str {
        match self {
            Self::Codex => {
                guide_url!("docs/connections.md#codex")
            }
            Self::Claude => {
                guide_url!("docs/connections.md#claude-code")
            }
            Self::Other => CLIENT_GUIDE,
        }
    }
}

#[derive(PartialEq, Eq)]
struct VerificationTarget {
    data_dir: PathBuf,
    app_dir: PathBuf,
    endpoint: String,
    attach_only: bool,
    settings: Option<Settings>,
}

impl VerificationTarget {
    fn capture(installed: &Installed) -> Self {
        Self {
            data_dir: installed.data_dir.clone(),
            app_dir: installed.app_dir.clone(),
            endpoint: installed.mcp_url.clone(),
            attach_only: installed.attach_only,
            settings: setup::saved_install_settings(installed).ok(),
        }
    }

    fn is_current(&self) -> bool {
        setup::existing_install_at(&self.data_dir)
            .as_ref()
            .map(Self::capture)
            .as_ref()
            == Some(self)
    }
}

struct ControlsState {
    data_dir: Option<PathBuf>,
    installation: Option<VerificationTarget>,
    backend_url: Option<String>,
    model_root: PathBuf,
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
    model_manager: model_ui::ModelManager,
    workspace_tab: WorkspaceTab,
    compact_window_size: Option<Vec2>,
    pipelines: pipeline_ui::PipelinePanel,
    nodes: node_view::NodePanel,
    model_destination_error: Option<String>,
    python_check_failed: bool,
    #[cfg(windows)]
    python_install_offer: bool,
    #[cfg(windows)]
    python_installing: bool,
    #[cfg(windows)]
    python_install_error: Option<String>,
    settings: Settings,
    setup_profile: Option<Settings>,
    using_saved_setup: bool,
    setup_config_error: Option<String>,
    check: Option<CheckOutcome>,
    worker: Option<Receiver<Event>>,
    progress: String,
    install_error: Option<String>,
    install_cancel: Option<Arc<AtomicBool>>,
    installed: Option<Installed>,
    copied: Option<&'static str>,
    connect_target: ConnectTarget,
    connect_client: ConnectClient,
    connect_profile: String,
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
    first_render: first_render::FirstRender,
    #[cfg(windows)]
    finish_requested: bool,
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
    uninstall: uninstall_ui::UninstallDialog,
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
        theme::configure_for(ctx, appearance, data_dir);
        let mut installer = Self {
            step: Step::Prerequisites,
            python: None,
            selected_python: None,
            installation_dir: None,
            install_assets: asset_ui::AssetEditor::default(),
            model_manager: model_ui::ModelManager::default(),
            workspace_tab: WorkspaceTab::Jobs,
            compact_window_size: None,
            pipelines: pipeline_ui::PipelinePanel::default(),
            nodes: node_view::NodePanel::default(),
            model_destination_error: None,
            python_check_failed: false,
            #[cfg(windows)]
            python_install_offer: false,
            #[cfg(windows)]
            python_installing: false,
            #[cfg(windows)]
            python_install_error: None,
            settings: setup::default_settings(),
            setup_profile: None,
            using_saved_setup: false,
            setup_config_error: None,
            check: None,
            worker: None,
            progress: String::new(),
            install_error: None,
            install_cancel: None,
            installed: None,
            copied: None,
            connect_target: ConnectTarget::default(),
            connect_client: ConnectClient::default(),
            connect_profile: String::new(),
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
            first_render: first_render::FirstRender::default(),
            #[cfg(windows)]
            finish_requested: false,
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
            uninstall: uninstall_ui::UninstallDialog::default(),
            #[cfg(windows)]
            update: update_ui::UpdateDialog::default(),
        };
        match mode {
            LaunchMode::Setup(launch) => {
                installer.installation_dir = launch.directory;
                if launch.invalid {
                    installer.setup_config_error = Some(
                        "Use --config FILE and optionally --data-dir DIRECTORY to prefill setup."
                            .into(),
                    );
                }
                if let Ok(data) = installer.setup_directory() {
                    installer.install_assets.load(&data);
                    if !launch.invalid {
                        installer.use_setup_destination(&data);
                    }
                }
                if !launch.invalid
                    && let Some(path) = launch.config
                {
                    installer.import_setup_config(&path);
                }
                installer.check_python(ctx);
            }
            LaunchMode::Controls { target, .. } => {
                let backend_url = match &target {
                    MonitorTarget::ComfyUrl(url) => Some(url.clone()),
                    _ => None,
                };
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
                    installation: None,
                    backend_url,
                    model_root: PathBuf::new(),
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

    fn setup_directory(&self) -> Result<PathBuf, String> {
        self.installation_dir
            .clone()
            .map_or_else(setup::data_dir, Ok)
    }

    fn use_setup_destination(&mut self, data: &Path) {
        let fallback = self.setup_profile.clone().unwrap_or_else(|| {
            if self.using_saved_setup {
                setup::default_settings()
            } else {
                self.settings.clone()
            }
        });
        let result = setup::existing_setup_config(data).and_then(|saved| {
            let using_saved = saved.is_some();
            let settings = saved.map_or_else(
                || fallback.clone(),
                |saved| setup::fill_missing(saved, &fallback),
            );
            Ok((settings, using_saved))
        });
        match result {
            Ok((settings, using_saved)) => {
                self.setup_config_error = None;
                self.using_saved_setup = using_saved;
                if settings != self.settings {
                    self.settings = settings;
                    self.settings_changed();
                    if self.step == Step::Install {
                        self.change_step(Step::Comfy);
                    }
                }
            }
            Err(error) => self.setup_config_error = Some(error),
        }
    }

    fn import_setup_config(&mut self, path: &Path) {
        let load = setup::import_setup_config(path, &setup::default_settings())
            .and_then(|profile| Ok((self.setup_directory()?, profile)));
        match load {
            Ok((data, profile)) => {
                self.setup_profile = Some(profile);
                self.use_setup_destination(&data);
            }
            Err(error) => self.setup_config_error = Some(error),
        }
    }

    fn setup_config_picker(&mut self, ui: &mut Ui, idle: bool, show_privacy: bool) {
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Load setup config", idle).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Render Bridge settings", &["json"])
                    .pick_file()
            {
                self.import_setup_config(&path);
            }
            if show_privacy {
                ui.hyperlink_to("Privacy and network use", PRIVACY_GUIDE);
            }
        });
        if let Some(error) = &self.setup_config_error {
            status(ui, "Setup config unavailable", error, CAUTION);
            if secondary(ui, "Continue without imported config", idle).clicked() {
                self.setup_config_error = None;
            }
        }
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
        if !matches!(action, ServerAction::Exit)
            && self
                .installed
                .as_ref()
                .is_some_and(|installed| !installed.may_manage_server())
        {
            return;
        }
        if matches!(action, ServerAction::Exit) && self.model_manager.downloading() {
            self.select_workspace(ctx, WorkspaceTab::Models);
            self.queue_dialog = None;
            self.model_manager.cancel();
            return;
        }
        if self.worker.is_some() || self.update.blocks_controls() {
            return;
        }
        if !matches!(action, ServerAction::StartSilently)
            && let Some(setting) = &mut self.startup_setting
        {
            setting.open = false;
        }
        match action {
            ServerAction::Start | ServerAction::StartSilently => {
                self.queue_dialog = None;
                self.start_panel_server(ctx, matches!(action, ServerAction::StartSilently));
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
                QueueAction::StopBridge => "Stop Render Bridge?",
                QueueAction::CloseWindow => "Stop Render Bridge and exit?",
            };
            ui.heading(RichText::new(title).size(25.0).strong());
            let footer_space = if action == QueueAction::Inspect { 116.0 } else { 164.0 };
            egui::ScrollArea::vertical()
                .id_salt("jobs-dialog-body")
                .auto_shrink([false, true])
                .max_height((ctx.content_rect().height() - footer_space).max(72.0))
                .show(ui, |ui| dialog.queue.show(ui));
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
                let can_stop = label.is_some()
                    && !(action == QueueAction::CloseWindow && self.model_manager.downloading());
                stop = if in_panel {
                    monitor_button(ui, label_text, can_stop, ERROR)
                } else {
                    destructive(ui, label_text, can_stop)
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
        let effective = self
            .setup_directory()
            .and_then(|data| setup::settings_for_setup(&data, &self.settings));
        let settings = match effective {
            Ok(settings) => settings,
            Err(error) => {
                self.check = Some(CheckOutcome::Failed(error));
                self.manual_confirmed = false;
                return;
            }
        };
        self.settings = settings.clone();
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
        let Some(installed) = self.installed.clone().filter(Installed::may_manage_server) else {
            return;
        };
        self.verification = None;
        self.progress = "Starting the Bridge and checking authenticated MCP…".into();
        let target = VerificationTarget::capture(&installed);
        let running = self.running.take();
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
                #[cfg(windows)]
                let (running, result) = (
                    running,
                    connection::start_installed_and_verify(&installed, &clients, &codex_profile),
                );
                #[cfg(not(windows))]
                let (running, result) = {
                    let mut running = running;
                    let result = connection::start_and_verify(
                        &installed,
                        &mut running,
                        &clients,
                        &codex_profile,
                    );
                    (running, result)
                };
                let _ = sender.send(Event::Verified(target, running, result));
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

    #[cfg(windows)]
    fn finish_ready(&self) -> bool {
        self.verification.as_ref().is_some_and(|result| {
            result.as_ref().is_ok_and(|result| {
                matches!(
                    result["bridge"].as_str(),
                    Some("ok" | "comfyui_unavailable")
                )
            })
        })
    }

    #[cfg(windows)]
    fn panel_manages_listener(&self) -> Option<bool> {
        self.verification.as_ref()?.as_ref().ok()?["panel_managed"].as_bool()
    }

    fn server_ready_for_render(&self) -> bool {
        self.verification.as_ref().is_some_and(|result| {
            result.as_ref().is_ok_and(|result| {
                matches!(result["bridge"].as_str(), Some("ok" | "already_running"))
                    && result["folders"].as_bool() == Some(true)
            })
        })
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
                .map(|current| (&current.app_dir, &current.pause_flag))
                != self
                    .installed
                    .as_ref()
                    .map(|opened| (&opened.app_dir, &opened.pause_flag))
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
                    .env("PAUSE_FLAG", &installed.pause_flag)
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
    fn start_panel_server(&mut self, ctx: &egui::Context, force_start: bool) {
        self.start_or_verify_panel(ctx, force_start, false);
    }

    fn start_or_verify_panel(
        &mut self,
        ctx: &egui::Context,
        force_start: bool,
        verify_client: bool,
    ) {
        let Some(installed) = self.installed.clone().filter(Installed::may_manage_server) else {
            return;
        };
        let owned = self.running.as_mut().is_some_and(RunningBridge::is_running);
        if owned && !verify_client {
            if self.verification.is_none() {
                self.verification = Some(Ok(serde_json::json!({ "bridge": "already_running" })));
            }
            return;
        }
        #[cfg(windows)]
        let confirm_only = owned
            || (!force_start
                && self
                    .controls
                    .as_ref()
                    .is_some_and(|controls| controls.bridge_online));
        #[cfg(not(windows))]
        let confirm_only = {
            let _ = force_start;
            owned
        };

        let client = verify_client
            .then(|| self.connect_client.verifier())
            .flatten();
        let clients: Vec<_> = client.into_iter().collect();
        let codex_profile = if client == Some(Client::Codex) {
            self.connect_profile.trim().to_owned()
        } else {
            String::new()
        };
        self.verification = None;
        self.progress = if verify_client {
            "Checking the authenticated MCP connection…".into()
        } else {
            "Checking the Render Bridge server…".into()
        };
        if !owned {
            self.running = None;
        }
        let target = VerificationTarget::capture(&installed);
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("render-bridge-panel-start".into())
            .spawn(move || {
                let mut running = None;
                let result = if connection::bridge_is_listening(&installed) {
                    connection::verify_listener(&installed, &clients, &codex_profile).and_then(
                        |mut result| {
                            if result["bridge"] == "ok" {
                                let settings = setup::saved_install_settings(&installed)?;
                                result["folders"] = setup::recheck_folders(&settings)?.into();
                                result["bridge"] = "already_running".into();
                            }
                            Ok(result)
                        },
                    )
                } else if confirm_only {
                    Err("The server no longer responds. Check the status and try again.".into())
                } else {
                    connection::start_and_verify(&installed, &mut running, &clients, &codex_profile)
                };
                let _ = sender.send(Event::Verified(target, running, result));
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
                    let settings = setup::confirm_setup_settings(&data, &settings)?;
                    // Stop setup if saved storage preferences cannot be read.
                    asset_folders::load(&data)?;
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
                Ok(Event::Verified(target, running, result)) => {
                    self.worker = None;
                    if let Some(running) = running {
                        self.running = Some(running);
                    }
                    if !target.is_current() {
                        self.verification = Some(Err("Render Bridge settings changed during verification. Check the connection again.".into()));
                        self.first_render = first_render::FirstRender::default();
                        continue;
                    }
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
                    if result.is_ok() {
                        self.verification = None;
                        self.first_render = first_render::FirstRender::default();
                    }
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
                            if let Some(folder) = self.model_manager.destination_change() {
                                self.model_destination_error =
                                    model_library::save_destination(&installed.data_dir, folder)
                                        .err();
                            }
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

    fn setup_titlebar(&self, ctx: &egui::Context) {
        let rail = theme::color(ctx, RAIL);
        let line = theme::color(ctx, LINE);
        egui::TopBottomPanel::top("setup-titlebar")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(rail)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    bridge_mark(ui);
                    ui.label(
                        RichText::new("Render Bridge Setup")
                            .size(17.0)
                            .strong()
                            .color(theme::on_fill(rail)),
                    );
                });
                let y = ui.min_rect().bottom();
                let x = ctx.content_rect().x_range();
                ui.painter().hline(x, y, Stroke::new(1.0_f32, line));
            });
    }

    fn rail(&self, ctx: &egui::Context) {
        let width = if ctx.content_rect().width() < 850.0 {
            154.0
        } else {
            178.0
        };
        egui::SidePanel::left("setup-steps")
            .exact_width(width)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, RAIL))
                    .stroke(Stroke::new(1.0_f32, theme::color(ctx, LINE)))
                    .inner_margin(egui::Margin::symmetric(11, 16)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                for (step, label) in STEPS {
                    let selected = step == self.step;
                    let fill = if selected {
                        theme::color(ctx, LIME)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let foreground = if selected {
                        theme::on_fill(fill)
                    } else {
                        theme::color(ctx, RAIL_TEXT)
                    };
                    Frame::new()
                        .fill(fill)
                        .corner_radius(9)
                        .inner_margin(egui::Margin::symmetric(12, 11))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(RichText::new(label).size(15.0).strong().color(foreground));
                        });
                }
            });
    }

    fn prerequisites_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Get ready to render",
            "Set up Python, a local ComfyUI build, and an agent.",
        );
        let idle = self.worker.is_none();
        let ready = self.python_ready();
        let python_status = if self.python_check_failed {
            "Python check failed".into()
        } else if !idle {
            #[cfg(windows)]
            let installing = self.python_installing;
            #[cfg(not(windows))]
            let installing = false;
            if installing {
                "Waiting for Windows Python setup…".into()
            } else {
                "Checking Python…".into()
            }
        } else {
            match &self.python {
                Some(PythonReadiness::Ready { version, .. }) => {
                    format!("Detected · Python {version}")
                }
                Some(PythonReadiness::TooOld { version }) => format!("Python {version} is too old"),
                Some(PythonReadiness::MissingVenv { version }) => {
                    format!("Python {version} needs virtual environment support")
                }
                Some(PythonReadiness::Missing) => "Python was not found".into(),
                Some(PythonReadiness::InvalidSelection) => {
                    "Could not verify the selected executable".into()
                }
                None => "Requires Python 3.11 or newer".into(),
            }
        };
        let needs_python = idle && !ready && (self.python.is_some() || self.python_check_failed);
        wizard_readiness(
            ui,
            "Python",
            &python_status,
            if ready && idle {
                GREEN
            } else if needs_python {
                CAUTION
            } else {
                MUTED
            },
            |ui| {
                if needs_python {
                    let missing_venv =
                        matches!(self.python, Some(PythonReadiness::MissingVenv { .. }));
                    let install_help = if missing_venv {
                        python_venv_help()
                    } else {
                        "Install Python 3.11 or newer."
                    };
                    let recheck_same = self.selected_python.is_none()
                        || (missing_venv && cfg!(target_os = "linux"));
                    let next = if recheck_same {
                        "Then select Check Python again."
                    } else {
                        "Then select Detect installed Python."
                    };
                    caption(ui, &format!("{install_help} {next}"));
                    ui.horizontal_wrapped(|ui| {
                        #[cfg(windows)]
                        if setup::winget_available()
                            && secondary(ui, "Install Python with WinGet", idle).clicked()
                        {
                            self.python_install_offer = true;
                        }
                        let (label, url) = python_install_link();
                        ui.hyperlink_to(label, url);
                        if self.selected_python.is_some()
                            && secondary(ui, "Detect installed Python", idle).clicked()
                        {
                            self.selected_python = None;
                            self.check_python(ui.ctx());
                        }
                        #[cfg(target_os = "linux")]
                        if secondary(ui, "Copy Debian or Ubuntu command", true).clicked() {
                            ui.ctx().copy_text(
                                "sudo apt update && sudo apt install python3 python3-venv".into(),
                            );
                            self.copied = Some(
                                "Run the copied command in a terminal, then check Python again.",
                            );
                        }
                    });
                }
                #[cfg(windows)]
                if let Some(message) = &self.python_install_error {
                    caption(ui, message);
                }
                ui.collapsing("Python options", |ui| {
                    if let Some(PythonReadiness::Ready { executable, .. }) = &self.python {
                        code_label(ui, &executable.to_string_lossy());
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
                            && !needs_python
                            && secondary(ui, "Use automatic detection", idle).clicked()
                        {
                            self.selected_python = None;
                            self.check_python(ui.ctx());
                        } else if ready && secondary(ui, "Check Python again", idle).clicked() {
                            self.check_python(ui.ctx());
                        }
                    });
                });
                if let Some(message) = self.copied {
                    caption(ui, message);
                }
            },
        );

        let paired = matches!(self.check, Some(CheckOutcome::Models(_, true, _)));
        let comfy_status = if paired {
            "Local build and folders verified"
        } else if !self.instances.is_empty() {
            "Local build detected"
        } else {
            "Choose a build on the next screen"
        };
        wizard_readiness(
            ui,
            "ComfyUI",
            comfy_status,
            if paired { GREEN } else { MUTED },
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Get Comfy Desktop from Comfy Org", COMFY_DOWNLOAD);
                    ui.hyperlink_to("ComfyUI manual setup guide", COMFY_GUIDE);
                });
            },
        );

        let claude = connection::profile_path(Client::Claude).is_some_and(|path| path.is_file());
        let codex = connection::profile_path(Client::Codex).is_some_and(|path| path.is_file());
        let agent_status = match (claude, codex) {
            (true, true) => "Claude Code and Codex settings found",
            (true, false) => "Claude Code settings found",
            (false, true) => "Codex settings found",
            (false, false) => "Connect an MCP client after installation",
        };
        wizard_readiness(ui, "Agent", agent_status, MUTED, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Set up Claude Code", CLAUDE_SETUP);
                ui.hyperlink_to("Set up Codex", CODEX_SETUP);
            });
        });
        self.setup_config_picker(ui, idle, true);
        ui.collapsing("Existing installations", |ui| {
            #[cfg(windows)]
            caption(
                ui,
                "To repair Render Bridge, exit the tray app, stop its server, and run setup with the same ComfyUI build and installation folder.",
            );
            ui.horizontal_wrapped(|ui| {
                if let Some(installed) = self
                    .setup_directory()
                    .ok()
                    .and_then(|data| setup::existing_install_at(&data))
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
                        self.install_error = Some(
                            "That folder does not contain a verified Render Bridge setup.".into(),
                        );
                    }
                }
                #[cfg(windows)]
                if secondary(ui, "Uninstall Render Bridge", idle).clicked() {
                    self.uninstall.choose();
                }
            })
        });
        if self.setup_config_error.is_none()
            && (self.using_saved_setup || self.setup_profile.is_some())
        {
            caption(
                ui,
                "Setup will use the saved ComfyUI settings. Existing values take precedence.",
            );
        }
        if let Some(error) = &self.install_error {
            caption(ui, error);
        }
        #[cfg(windows)]
        if self.python_install_offer {
            let prompt = panel_modal(ui.ctx(), "python-install-approval").show(ui.ctx(), |ui| {
                ui.set_width((ui.ctx().content_rect().width() - 32.0).clamp(120.0, 360.0));
                utility_ui::heading(ui, utility_ui::UtilityIcon::Python, "Install Python?", Some("Administrator approval required"));
                caption(ui, "WinGet asks Windows to install the latest available Python 3.13 update for all users.");
                let (label, url) = python_install_link();
                ui.hyperlink_to(label, url);
                ui.horizontal_wrapped(|ui| {
                    let cancel = secondary(ui, "Cancel", true).clicked();
                    let approve = primary(ui, "Request administrator approval", true).clicked();
                    (approve, cancel)
                }).inner
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
            "Choose a ComfyUI build",
            "Select the local build you want Render Bridge to use.",
        );
        let idle = self.worker.is_none();
        let mut changed = false;
        if !self.instances.is_empty() {
            let mut chosen = None;
            for (index, instance) in self.instances.iter().enumerate() {
                let selected = instance.url == self.settings.comfy_url
                    && setup::folder(&self.settings, "input") == instance.input
                    && setup::folder(&self.settings, "output") == instance.output;
                let stroke = if selected { VIOLET } else { LINE };
                Frame::new()
                    .fill(theme::color(ui.ctx(), SURFACE))
                    .stroke(Stroke::new(
                        if selected { 2.0_f32 } else { 1.0_f32 },
                        theme::color(ui.ctx(), stroke),
                    ))
                    .corner_radius(13)
                    .inner_margin(egui::Margin::symmetric(14, 13))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    idle,
                                    egui::RadioButton::new(
                                        selected,
                                        theme::strong("ComfyUI", 18.0),
                                    ),
                                )
                                .clicked()
                            {
                                chosen = Some(index);
                            }
                            code_label(ui, &instance.url);
                            wizard_tag(ui, "Detected", GREEN);
                        });
                        if let Some(root) = &instance.root {
                            code_label(ui, &root.to_string_lossy());
                        } else {
                            caption(ui, &format!("Input · {}", instance.input.display()));
                            caption(ui, &format!("Output · {}", instance.output.display()));
                        }
                    });
            }
            if let Some(index) = chosen {
                self.use_instance(index);
            }
        } else if self.discovery_done {
            caption(
                ui,
                "No running instance reported its folders. Start ComfyUI or enter the build's address and folders.",
            );
        }
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Detect running ComfyUI", idle).clicked() {
                self.start_discovery(ui.ctx());
            }
            if secondary(ui, "Check jobs", idle).clicked() {
                self.show_jobs(QueueAction::Inspect);
            }
        });
        self.setup_config_picker(ui, idle, false);
        egui::CollapsingHeader::new("ComfyUI address and folders")
            .default_open(self.instances.is_empty())
            .show(ui, |ui| {
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
                        changed |=
                            text_edit(ui, &mut self.settings.comfy_url, url_width, url_label);
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
                })
            });
        if changed {
            self.settings_changed();
        }
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            if self.can_continue() && secondary(ui, "Check again", idle).clicked() {
                self.start_check(ui.ctx());
            }
            ui.hyperlink_to("Get Comfy Desktop from Comfy Org", COMFY_DOWNLOAD);
            ui.hyperlink_to("ComfyUI manual setup guide", COMFY_GUIDE);
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

    fn installed_connect(&mut self, ui: &mut Ui) {
        theme::heading(ui, "Connect an agent", 27.0);
        let Some(installed) = self.installed.clone() else {
            status(
                ui,
                "Installation unavailable",
                "Open Render Bridge Setup to locate or repair the installation.",
                CAUTION,
            );
            return;
        };
        if installed.attach_only {
            #[cfg(windows)]
            let online = self
                .controls
                .as_ref()
                .map(|controls| controls.bridge_online);
            #[cfg(not(windows))]
            let online = None;
            attached_connect(ui, online);
            return;
        }
        let idle = self.worker.is_none() && !self.update_in_progress();
        ui.horizontal_wrapped(|ui| {
            for (target, label) in [
                (ConnectTarget::Local, "This computer"),
                (ConnectTarget::Network, "Another device"),
            ] {
                if connect_option(ui, label, self.connect_target == target, idle, CREAM) {
                    self.connect_target = target;
                    self.copied = None;
                    self.confirm_rotation = false;
                }
            }
        });

        if self.connect_target == ConnectTarget::Network {
            self.network_connect(ui, &installed);
            return;
        }

        let owned = self.running.as_mut().is_some_and(RunningBridge::is_running);
        #[cfg(windows)]
        let online = owned
            || self
                .controls
                .as_ref()
                .is_some_and(|controls| controls.bridge_online);
        #[cfg(not(windows))]
        let online = owned;
        let can_rotate = idle && !online;
        if ui.available_width() >= 760.0 {
            ui.columns(2, |columns| {
                self.installed_client_fields(&mut columns[0], &installed, idle);
                self.installed_receipt_fields(&mut columns[1], &installed, can_rotate);
            });
        } else {
            self.installed_client_fields(ui, &installed, idle);
            self.installed_receipt_fields(ui, &installed, can_rotate);
        }
    }

    fn installed_client_fields(&mut self, ui: &mut Ui, installed: &Installed, idle: bool) {
        card(ui, |ui| {
            theme::heading(ui, "MCP client", 18.0);
            ui.horizontal_wrapped(|ui| {
                for client in [
                    ConnectClient::Codex,
                    ConnectClient::Claude,
                    ConnectClient::Other,
                ] {
                    if connect_option(
                        ui,
                        client.label(),
                        self.connect_client == client,
                        idle,
                        SURFACE,
                    ) {
                        self.connect_client = client;
                        self.copied = None;
                    }
                }
            });
            if self.connect_client == ConnectClient::Codex {
                ui.collapsing("Verify a Codex profile (optional)", |ui| {
                    let label = field_label(ui, "Profile name");
                    ui.add_enabled_ui(idle, |ui| {
                        let width = ui.available_width();
                        if text_edit(ui, &mut self.connect_profile, width, label)
                            && let Some(result) = self.verification.as_mut()
                            && let Some(entries) = result
                                .as_mut()
                                .ok()
                                .and_then(serde_json::Value::as_object_mut)
                        {
                            entries.remove(Client::Codex.key());
                        }
                    });
                });
            }
            ui.horizontal_wrapped(|ui| {
                if primary(ui, "Copy agent setup prompt", true).clicked() {
                    let preface = match self.connect_client {
                        ConnectClient::Other => String::new(),
                        client => format!("Use {} as my MCP client.\n\n", client.label()),
                    };
                    ui.ctx().copy_text(preface + &agent_setup_prompt(installed));
                    self.copied =
                        Some("Setup prompt copied. Paste it into your agent on this computer.");
                }
                ui.hyperlink_to("Manual setup", self.connect_client.guide());
            });
            ui.separator();
            if connect_copy_row(ui, "Local MCP endpoint", "Copy address", &installed.mcp_url) {
                self.copied = Some("MCP endpoint copied.");
            }
            ui.collapsing("Connection details", |ui| {
                if connect_copy_row(
                    ui,
                    "Private token file",
                    "Copy path",
                    &installed.token_path.to_string_lossy(),
                ) {
                    self.copied = Some("Private token file path copied.");
                }
            });
            if let Some(message) = self.copied {
                caption(ui, message);
            }
            ui.separator();
            if monitor_button(ui, "Start & Verify", idle, SURFACE)
                .on_hover_text("Verify the running server and the selected client settings")
                .clicked()
            {
                self.start_or_verify_panel(ui.ctx(), true, true);
            }
            if !idle && self.worker.is_some() {
                activity(ui, &self.progress);
            } else if let Some(result) = &self.verification {
                match result {
                    Ok(result) => {
                        let (message, passed) =
                            bridge_status(result["bridge"].as_str().unwrap_or_default());
                        connection_result(ui, "Render Bridge", message, passed);
                        if passed && result["folders"].as_bool() != Some(true) {
                            connection_result(
                                ui,
                                "ComfyUI folders",
                                "Folder pairing could not be verified.",
                                false,
                            );
                        }
                        if let Some(client) = self.connect_client.verifier()
                            && let Some(value) = result[client.key()].as_str()
                        {
                            let (message, passed) = client_status(value);
                            connection_result(ui, client.label(), message, passed);
                        }
                    }
                    Err(error) => connection_result(ui, "Verification", error, false),
                }
            }
        });
    }

    fn installed_receipt_fields(&mut self, ui: &mut Ui, installed: &Installed, can_rotate: bool) {
        let server_verified = self.server_ready_for_render();
        self.first_render.show(ui, installed, server_verified);
        ui.add_space(3.0);
        card(ui, |ui| {
            theme::heading(ui, "Private token", 18.0);
            caption(
                ui,
                if installed.token_path.is_file() {
                    "Stored in a local credential file."
                } else {
                    "Created when Render Bridge starts."
                },
            );
            if self.confirm_rotation {
                caption(
                    ui,
                    "Stop every server that uses this token file. Rotation revokes current credentials and signed links. Reconnect your agents after restarting Render Bridge.",
                );
                ui.horizontal_wrapped(|ui| {
                    if monitor_button(ui, "Cancel", true, SURFACE).clicked() {
                        self.confirm_rotation = false;
                    }
                    if monitor_button(ui, "Rotate private token", can_rotate, ERROR).clicked() {
                        self.confirm_rotation = false;
                        self.rotate_token(ui.ctx(), installed.clone());
                    }
                });
            } else if monitor_button(ui, "Rotate token…", can_rotate, SURFACE)
                .on_disabled_hover_text("Stop Render Bridge before rotating the token.")
                .clicked()
            {
                self.confirm_rotation = true;
            }
            if let Some(message) = &self.rotation_message {
                caption(ui, message);
            }
        });
    }

    fn network_connect(&mut self, ui: &mut Ui, installed: &Installed) {
        card(ui, |ui| {
            field_label(ui, "Connect over a private network");
            caption(
                ui,
                "Keep the render host's listener local. Use an SSH tunnel for one device or an HTTPS proxy on your private network.",
            );
            if let Some(port) = url::Url::parse(&installed.mcp_url)
                .ok()
                .and_then(|url| url.port_or_known_default())
            {
                let command = format!(
                    "ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:{port}:127.0.0.1:{port} user@render-host"
                );
                caption(
                    ui,
                    "On the client device, replace user@render-host with the render host's SSH login. Keep the tunnel open while you render.",
                );
                if connect_copy_row(ui, "SSH tunnel command", "Copy command", &command) {
                    self.copied = Some("Tunnel command copied.");
                }
                if connect_copy_row(
                    ui,
                    "MCP endpoint through the tunnel",
                    "Copy address",
                    &installed.mcp_url,
                ) {
                    self.copied = Some("Tunnel endpoint copied.");
                }
            }
            caption(
                ui,
                "Store the bearer token in the client device's private credential settings, then verify with server_status.",
            );
            ui.hyperlink_to("Set up access from another device", REMOTE_GUIDE);
            if let Some(message) = self.copied {
                caption(ui, message);
            }
        });
    }

    fn update_in_progress(&self) -> bool {
        #[cfg(windows)]
        return self.update.blocks_controls();
        #[cfg(not(windows))]
        false
    }

    fn models_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Choose a starter pack",
            "Download model files now or add them later in Models.",
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
        let root = Path::new(&self.settings.comfy_root).join("models");
        self.model_manager
            .show(ui, Some(&self.settings.comfy_url), root, None);
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
        ui.collapsing("Manual starter downloads", |ui| {
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
        });
    }

    fn install_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Install Render Bridge",
            "Choose where Render Bridge and your generated assets will live.",
        );
        let effective = self
            .setup_directory()
            .and_then(|data| setup::settings_for_setup(&data, &self.settings));
        if let Err(error) = &effective {
            status(ui, "Existing setup needs attention", error, CAUTION);
        }
        let effective = effective.unwrap_or_else(|_| self.settings.clone());
        card(ui, |ui| {
            ui.add_enabled_ui(self.worker.is_none(), |ui| {
                let default = setup::data_dir().ok();
                if asset_ui::compact_folder_option(
                    ui,
                    "Install folder",
                    &mut self.installation_dir,
                    &default.as_ref().map_or_else(
                        || "Default user folder".into(),
                        |path| path.to_string_lossy().into_owned(),
                    ),
                ) && let Some(data) = self.installation_dir.clone().or(default)
                {
                    self.install_assets.load(&data);
                    self.use_setup_destination(&data);
                }
                ui.separator();
                self.install_assets.fields(ui);
                if let Some(error) = &self.install_assets.message {
                    caption(ui, error);
                }
            });
        });
        ui.collapsing("Selected ComfyUI build", |ui| {
            code_label(ui, &effective.comfy_url);
            caption(
                ui,
                &format!("Input · {}", setup::folder(&effective, "input").display()),
            );
            caption(
                ui,
                &format!("Output · {}", setup::folder(&effective, "output").display()),
            );
        });
        caption(
            ui,
            "Setup downloads Python packages into Render Bridge's private environment.",
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
        if let Some(error) = &self.install_error {
            status(ui, "Configuration file", error, CAUTION);
        }
        if let Some(error) = &self.model_destination_error {
            status(ui, "Model folder choice was not saved", error, CAUTION);
        }
        if let Some(installed) = self
            .installed
            .clone()
            .filter(|installed| installed.attach_only)
        {
            theme::heading(ui, "Connect an agent", 27.0);
            attached_connect(ui, None);
            self.show_gaming_mode(ui, &installed);
            return;
        }
        let server_verified = self.server_ready_for_render();
        let receipt_verified = self
            .first_render
            .update_receipt(ui.ctx(), self.installed.as_ref());
        header(
            ui,
            if server_verified {
                "Ready to render"
            } else {
                "Connect your agent"
            },
            if server_verified && receipt_verified {
                "Your first render is verified."
            } else if server_verified {
                "Render Bridge and its ComfyUI folders are verified. Try a first render from your agent."
            } else {
                "Start Render Bridge and check the agent you want to connect."
            },
        );
        let Some(installed) = self.installed.clone() else {
            return;
        };
        if server_verified {
            self.first_render.show_current(ui, &installed, true);
        }
        let mut copied = None;
        let idle = self.worker.is_none();
        let mut selection_changed = false;
        card(ui, |ui| {
            field_label(ui, "Clients to verify");
            ui.horizontal_wrapped(|ui| {
                for (label, selected) in [
                    ("Codex", &mut self.verify_codex),
                    ("Claude Code", &mut self.verify_claude),
                ] {
                    if wizard_client_choice(ui, label, *selected, idle).clicked() {
                        *selected = !*selected;
                        selection_changed = true;
                    }
                }
            });
            copied = copy_agent_prompt(ui, &installed, idle).or(copied);
            if self.verify_claude || self.verify_codex {
                ui.collapsing("Client paths and profiles", |ui| {
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
                });
            }
            ui.collapsing("Endpoint and private token file", |ui| {
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
        });
        if selection_changed {
            self.verification = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("Connect an MCP client", CLIENT_GUIDE);
            ui.hyperlink_to("Connect from another device", REMOTE_GUIDE);
        });
        if !server_verified {
            self.first_render.show_current(ui, &installed, false);
        }
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
        if secondary(ui, "Check jobs", idle).clicked() {
            self.show_jobs(QueueAction::Inspect);
        }
        #[cfg(windows)]
        caption(
            ui,
            match self.panel_manages_listener() {
                Some(true) => {
                    "The installed panel keeps the server running when setup closes. Open Render Bridge to manage it."
                }
                Some(false) => {
                    "The existing server runs outside the panel. Keep its original launcher running to stay connected."
                }
                None => {
                    "Start & Verify starts the server in the installed panel or checks an existing server."
                }
            },
        );
        #[cfg(not(windows))]
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
        let current = installed.as_ref().map(VerificationTarget::capture);
        let changed = current != controls.installation;
        controls.backend_url = url.clone();
        if changed {
            self.verification = None;
            self.first_render = first_render::FirstRender::default();
            controls.model_root = current
                .as_ref()
                .and_then(|current| current.settings.as_ref())
                .map(|settings| Path::new(&settings.comfy_root).join("models"))
                .unwrap_or_default();
        }
        #[cfg(windows)]
        {
            if changed {
                controls.bridge_probe = None;
                controls.bridge_online = false;
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
        controls.installation = current;
        self.installed = installed;
        controls.next_load = Instant::now() + Duration::from_secs(2);
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn select_workspace(&mut self, ctx: &egui::Context, tab: WorkspaceTab) {
        if self.workspace_tab == tab {
            return;
        }
        if self.workspace_tab == WorkspaceTab::Jobs {
            let (previous, monitor) = ctx.input(|input| {
                let viewport = input.viewport();
                (
                    viewport.inner_rect.map(|rect| rect.size()),
                    viewport.monitor_size,
                )
            });
            self.compact_window_size = previous;
            let available = monitor.map_or(egui::vec2(1000.0, 680.0), |size| {
                (size - egui::vec2(64.0, 96.0)).max(MONITOR_MIN_SIZE)
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                egui::vec2(1000.0, 680.0).min(available),
            ));
        } else if tab == WorkspaceTab::Jobs
            && let Some(previous) = self.compact_window_size.take()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                previous.max(MONITOR_MIN_SIZE),
            ));
        }
        if tab == WorkspaceTab::Connect {
            self.copied = None;
            self.confirm_rotation = false;
        }
        self.workspace_tab = tab;
    }

    fn panel_is_tray(&self) -> bool {
        #[cfg(windows)]
        return self.tray_panel.is_some();
        #[cfg(not(windows))]
        false
    }

    fn hide_or_close_controls(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        if let Some(tray) = &mut self.tray_panel {
            tray.hide(ctx);
            return;
        }
        if self.model_manager.downloading() {
            self.select_workspace(ctx, WorkspaceTab::Models);
            self.model_manager.cancel();
            return;
        }
        if self.running.as_mut().is_some_and(RunningBridge::is_running) {
            self.show_jobs(QueueAction::CloseWindow);
            return;
        }
        if self.worker.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    #[cfg(windows)]
    fn monitor_update_action(&mut self, ui: &mut Ui) {
        if self.installed.is_some()
            && self
                .controls
                .as_ref()
                .is_some_and(|controls| controls.update_available_here)
            && job_action_button(ui, "Update", true, SURFACE, JobActionGlyph::Update).clicked()
        {
            self.update.check(ui.ctx());
        }
    }

    fn jobs_footer(&mut self, ui: &mut Ui) {
        let idle = self.worker.is_none() && !self.update_in_progress();
        #[cfg(windows)]
        {
            let owned = self.running.as_mut().is_some_and(RunningBridge::is_running);
            let online = owned
                || self
                    .controls
                    .as_ref()
                    .is_some_and(|controls| controls.bridge_online);
            let width = ui.available_width();
            let compact = width < 620.0;
            let dense = width < COMPACT_MONITOR_WIDTH;
            ui.horizontal_wrapped(|ui| {
                if dense {
                    compact_monitor_spacing(ui);
                }
                if self
                    .installed
                    .as_ref()
                    .is_some_and(|installed| installed.attach_only)
                {
                    server_state(ui, online, false, compact);
                } else {
                    self.job_server_actions(ui, online, owned, idle, compact);
                }
                if !dense {
                    ui.separator();
                }
                self.job_gaming_action(ui, idle);
            });
        }
        #[cfg(not(windows))]
        ui.horizontal_wrapped(|ui| self.job_gaming_action(ui, idle));
    }

    #[cfg(windows)]
    fn job_server_actions(
        &mut self,
        ui: &mut Ui,
        online: bool,
        owned: bool,
        idle: bool,
        compact: bool,
    ) {
        server_state(ui, online, owned, compact);
        if !compact {
            ui.add_space(3.0);
        }
        let start_fill = if online { LINE } else { SURFACE };
        if job_action_button(
            ui,
            if compact { "Start" } else { "Start server" },
            idle,
            start_fill,
            JobActionGlyph::Start,
        )
        .on_hover_text(if online {
            "Confirm the server is running"
        } else {
            "Start Render Bridge"
        })
        .clicked()
        {
            self.request_server_action(ui.ctx(), ServerAction::Start);
        }
        let stop_fill = if owned { ERROR } else { LINE };
        if job_action_button(
            ui,
            if compact { "Stop" } else { "Stop server" },
            idle && owned,
            stop_fill,
            JobActionGlyph::Stop,
        )
        .on_disabled_hover_text(if online {
            "This panel can stop the server it starts."
        } else {
            "Start the server to enable Stop."
        })
        .clicked()
        {
            self.request_server_action(ui.ctx(), ServerAction::Stop);
        }
    }

    fn job_gaming_action(&mut self, ui: &mut Ui, idle: bool) {
        if let Some(installed) = &self.installed {
            match gaming_mode(installed) {
                Some(on) => {
                    if gaming_mode_button(ui, on, idle).clicked() {
                        self.change_gaming_mode(ui.ctx(), !on);
                    }
                }
                None => {
                    monitor_button(ui, "Gaming Mode unavailable", false, LINE)
                        .on_disabled_hover_text("Run the status command in a terminal.");
                }
            }
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    fn controls_screen(&mut self, ctx: &egui::Context) {
        if self.controls.as_ref().is_some_and(|controls| {
            controls.data_dir.is_some() && Instant::now() >= controls.next_load
        }) {
            self.load_controls(ctx);
        }
        egui::TopBottomPanel::top("controls-banner")
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, RAIL))
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ctx, |ui| {
                monitor_header(ui, |ui| {
                    let (close_label, hint) = if self.panel_is_tray() {
                        ("Hide", "Hide Render Bridge to the tray")
                    } else {
                        ("Close", "Close Render Bridge")
                    };
                    if monitor_button(ui, close_label, true, RAIL)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.hide_or_close_controls(ctx);
                    }
                    #[cfg(windows)]
                    if let Some(setting) = &mut self.startup_setting
                        && monitor_button(ui, "Settings", true, RAIL).clicked()
                    {
                        setting.open(ctx, self.installed.as_ref());
                    }
                    #[cfg(windows)]
                    self.monitor_update_action(ui);
                });
                ui.add_space(3.0);
                let compact = ui.available_width() < COMPACT_MONITOR_WIDTH;
                ui.horizontal_wrapped(|ui| {
                    if compact {
                        compact_monitor_spacing(ui);
                    }
                    let has_installation = self
                        .controls
                        .as_ref()
                        .is_some_and(|controls| controls.data_dir.is_some());
                    for (tab, label) in WORKSPACE_TABS {
                        let enabled = tab != WorkspaceTab::Connect || has_installation;
                        if connect_option(ui, label, self.workspace_tab == tab, enabled, RAIL) {
                            self.select_workspace(ctx, tab);
                        }
                    }
                });
            });
        if self.workspace_tab == WorkspaceTab::Jobs && self.installed.is_some() {
            egui::TopBottomPanel::bottom("controls-actions")
                .show_separator_line(true)
                .frame(
                    Frame::new()
                        .fill(theme::color(ctx, SURFACE))
                        .inner_margin(egui::Margin::symmetric(14, 6)),
                )
                .show(ctx, |ui| self.jobs_footer(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::color(ctx, CREAM)).inner_margin(egui::Margin::symmetric(14, 9)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().id_salt(self.workspace_tab).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = if self.workspace_tab == WorkspaceTab::Jobs { 5.0 } else { 7.0 };
                    if self.workspace_tab == WorkspaceTab::Connect {
                        self.installed_connect(ui);
                        return;
                    }
                    if self.workspace_tab != WorkspaceTab::Jobs {
                        if let Some(controls) = &self.controls {
                            let url = controls.backend_url.as_deref();
                            let data_dir = controls.data_dir.as_deref();
                            match self.workspace_tab {
                                WorkspaceTab::Models => {
                                    self.model_manager.show_workspace(
                                        ui, url, controls.model_root.clone(), data_dir,
                                    );
                                    if let Some(request) = self.model_manager.take_pipeline_request() {
                                        match request {
                                            model_ui::PipelineRequest::BuiltInWan => self.pipelines.show_builtin(),
                                            model_ui::PipelineRequest::InstalledModel { category, name } => {
                                                if let Some(backend) = url {
                                                    self.pipelines.use_model(backend.to_owned(), category, name);
                                                }
                                            }
                                        }
                                        self.select_workspace(ctx, WorkspaceTab::Pipelines);
                                    }
                                }
                                WorkspaceTab::Pipelines => {
                                    self.pipelines.show(ui, url, data_dir, self.installed.as_ref());
                                }
                                WorkspaceTab::Nodes => self.nodes.show(ui, url, self.installed.as_ref()),
                                WorkspaceTab::Jobs | WorkspaceTab::Connect => {}
                            }
                        }
                        return;
                    }
                    if let Some(queue) = self.controls.as_mut().and_then(|controls| controls.queue.as_mut()) {
                        queue.show_card(ui);
                    } else {
                        status(ui, "Queue status unavailable",
                            "The saved ComfyUI connection could not be read. Open Render Bridge Setup to review the installation.", CAUTION);
                    }
                    if let Some(error) = &self.gaming_error {
                        connection_result(ui, "Gaming Mode", error, false);
                    }
                    if self.worker.is_some() {
                        activity(ui, &self.progress);
                    }
                    #[cfg(windows)]
                    if let Some(verification) = &self.verification {
                        match verification {
                            Ok(result) => {
                                let (message, passed) = bridge_status(result["bridge"].as_str().unwrap_or_default());
                                connection_result(ui, "Render Bridge", message, passed);
                            }
                            Err(message) => connection_result(ui, "Render Bridge server", message, false),
                        }
                    }
                    #[cfg(windows)]
                    if let Some(error) = &self.tray_error {
                        status(ui, "Tray icon unavailable", error, CAUTION);
                    }
                });
            });
        #[cfg(windows)]
        if let Some(setting) = &mut self.startup_setting {
            if self.tray_panel.as_ref().is_some_and(|tray| tray.hidden) {
                setting.open = false;
            } else {
                let data_dir = self
                    .controls
                    .as_ref()
                    .and_then(|controls| controls.data_dir.as_deref());
                setting.show(ctx, &mut self.appearance, data_dir, self.installed.as_ref());
                if setting.take_model_destination_change() {
                    self.model_manager.reload_saved_destination();
                }
            }
        }
        #[cfg(windows)]
        {
            let blocked = if self.worker.is_some() || self.model_manager.downloading() {
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
            .exact_height(65.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .stroke(Stroke::new(1.0_f32, theme::color(ctx, LINE)))
                    .inner_margin(egui::Margin::symmetric(20, 10)),
            )
            .show(ctx, |ui| {
                let idle = self.worker.is_none() && !self.model_manager.downloading();
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
                        if self.running.is_none()
                            && let Some(installed) = &self.installed
                            && secondary(ui, "Change ComfyUI", true).on_hover_text("Open settings.json. Stop Render Bridge before editing the saved configuration.").clicked()
                        {
                            self.install_error = open_configuration_file(installed).err();
                            self.verification = None;
                            self.first_render = first_render::FirstRender::default();
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
                            Step::Prerequisites if self.python_ready() => (
                                "Continue to ComfyUI",
                                idle && self.setup_config_error.is_none(),
                            ),
                            Step::Prerequisites => ("Check Python again", idle),
                            Step::Comfy if self.can_continue() => {
                                ("Continue", idle && self.setup_config_error.is_none())
                            }
                            Step::Comfy => (
                                "Check ComfyUI",
                                idle && self.setup_config_error.is_none()
                                    && !self.settings.comfy_root.trim().is_empty()
                                    && !self.settings.comfy_url.trim().is_empty(),
                            ),
                            Step::Models => ("Continue to install", self.can_continue() && idle),
                            Step::Install if self.install_error.is_some() => {
                                ("Retry installation", idle)
                            }
                            Step::Install => ("Install Render Bridge", idle),
                            Step::Connect if self.installed.as_ref().is_some_and(|installed| installed.attach_only) => {
                                ("Close setup", idle)
                            }
                            #[cfg(windows)]
                            Step::Connect if self.finish_ready() => (
                                if self.panel_manages_listener() == Some(true) {
                                    "Finish and keep running"
                                } else {
                                    "Finish setup"
                                },
                                idle,
                            ),
                            Step::Connect if self.running.is_some() => ("Verify again", idle),
                            Step::Connect => ("Start & Verify", idle),
                        };
                        if primary(ui, label, ready).clicked() {
                            match self.step {
                                Step::Prerequisites if self.python_ready() => {
                                    self.change_step(Step::Comfy);
                                    if self.instances.is_empty()
                                        && !self.using_saved_setup
                                        && self.setup_profile.is_none()
                                    {
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
                                Step::Connect if self.installed.as_ref().is_some_and(|installed| installed.attach_only) => {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                Step::Connect => {
                                    #[cfg(windows)]
                                    {
                                        self.finish_requested = self.finish_ready();
                                    }
                                    self.start_verification(ctx);
                                }
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
        #[cfg(windows)]
        if self.finish_requested && self.worker.is_none() {
            self.finish_requested = false;
            if self.finish_ready() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        self.model_manager.update(ctx);
        let can_hide = {
            #[cfg(windows)]
            {
                self.tray_panel.is_some()
            }
            #[cfg(not(windows))]
            {
                false
            }
        };
        if self.model_manager.downloading()
            && ctx.input(|input| input.viewport().close_requested())
            && !can_hide
        {
            self.model_manager.cancel();
            self.queue_dialog = None;
            if self.controls.is_some() {
                self.select_workspace(ctx, WorkspaceTab::Models);
            } else {
                self.change_step(Step::Models);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
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
                    || self.model_manager.downloading()
                    || self.update.blocks_controls()
                    || self.running.as_mut().is_some_and(RunningBridge::is_running);
                let hidden = self
                    .tray_panel
                    .as_mut()
                    .is_some_and(|tray| tray.update(ctx, guard_exit));
                let busy = self.worker.is_some() || self.update.blocks_controls();
                let action = self.tray_panel.as_mut().and_then(|tray| {
                    if busy && matches!(tray.server_action, Some(ServerAction::StartSilently)) {
                        return None;
                    }
                    tray.server_action.take()
                });
                if let Some(action) = action {
                    self.request_server_action(ctx, action);
                }
                if hidden {
                    return;
                }
            }
            #[cfg(windows)]
            if self.tray_panel.is_none() && ctx.input(|input| input.viewport().close_requested()) {
                if self.model_manager.downloading()
                    || self.worker.is_some()
                    || self.update.blocks_controls()
                {
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
        #[cfg(windows)]
        if close_requested && self.uninstall.is_running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if matches!(self.step, Step::Install | Step::Connect)
            && self.worker.is_some()
            && close_requested
        {
            if let Some(cancel) = &self.install_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if close_requested
            && !self.model_manager.downloading()
            && self.running.as_mut().is_some_and(RunningBridge::is_running)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self
                .queue_dialog
                .as_ref()
                .is_none_or(|dialog| dialog.action != QueueAction::CloseWindow)
            {
                self.show_jobs(QueueAction::CloseWindow);
            }
        }
        self.setup_titlebar(ctx);
        self.rail(ctx);
        self.actions(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(theme::color(ctx, CREAM))
                    .inner_margin(egui::Margin::symmetric(32, 22)),
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
        #[cfg(windows)]
        self.uninstall.show(ctx);
    }
}

fn attached_connect(ui: &mut Ui, online: Option<bool>) {
    card(ui, |ui| {
        let (title, tone) = match online {
            Some(true) => ("External server online", GREEN),
            Some(false) => ("External server offline", CAUTION),
            None => ("External server", MUTED),
        };
        status(
            ui,
            title,
            "Connect your agent using the external server’s address and credentials.",
            tone,
        );
        ui.hyperlink_to("Connect an MCP client", CLIENT_GUIDE);
    });
}

fn open_configuration_file(installed: &Installed) -> Result<(), String> {
    let path = setup::config_file(installed);
    let (program, args): (&str, &[&str]) = if cfg!(windows) {
        ("notepad.exe", &[])
    } else if cfg!(target_os = "macos") {
        ("open", &["-t"])
    } else {
        ("xdg-open", &[])
    };
    setup::quiet_command(program)
        .args(args)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not open settings.json in a text editor.".into())
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
        "Use the full Python installer from python.org."
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
        "owner_unverified" => (
            "The listener on this port could not prove it belongs to this installation. If you just repaired Render Bridge, exit its previous panel or terminal and retry. For another service, choose a free MCP port.",
            false,
        ),
        "backend_mismatch" => (
            "The existing server uses a different ComfyUI address or asset folders. Stop it from its launcher before starting Render Bridge with the selected settings.",
            false,
        ),
        _ => (
            "Could not verify authenticated MCP. Check the Bridge and try again.",
            false,
        ),
    }
}

fn gaming_mode(installed: &Installed) -> Option<bool> {
    installed.pause_flag.try_exists().ok()
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
    theme::heading(ui, title, 32.0);
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
        (
            24.0,
            3.0,
            theme::color(ui.ctx(), MARK_SHADE).gamma_multiply(0.67),
        ),
        (13.0, 13.0, theme::color(ui.ctx(), MARK_SHADE)),
        (2.0, 23.0, theme::color(ui.ctx(), GREEN)),
    ] {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(bounds.min + egui::vec2(x, y), egui::vec2(19.0, 19.0)),
            7,
            color,
        );
    }
}

fn wizard_client_choice(ui: &mut Ui, title: &str, selected: bool, enabled: bool) -> egui::Response {
    let fill = theme::color(ui.ctx(), if selected { RAIL_ACTIVE } else { SURFACE });
    let border = theme::color(ui.ctx(), if selected { VIOLET } else { LINE });
    ui.add_enabled(
        enabled,
        egui::Button::new(theme::strong(title, 17.0).color(theme::on_fill(fill)))
            .selected(selected)
            .fill(fill)
            .stroke(Stroke::new(1.5_f32, border))
            .corner_radius(9)
            .min_size(egui::vec2(154.0, 42.0)),
    )
}

fn wizard_tag(ui: &mut Ui, text: &str, tone: Color32) {
    let fill = theme::color(ui.ctx(), tone);
    Frame::new()
        .fill(fill)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(12.0)
                    .strong()
                    .color(theme::on_fill(fill)),
            )
        });
}

fn wizard_readiness(
    ui: &mut Ui,
    title: &str,
    label: &str,
    tone: Color32,
    controls: impl FnOnce(&mut Ui),
) {
    setup_card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
            let center = rect.center();
            let color = theme::color(ui.ctx(), tone);
            let painter = ui.painter();
            if tone == GREEN {
                painter.circle_filled(center, 15.0, color);
                let stroke = Stroke::new(2.4_f32, theme::on_fill(color));
                painter.add(egui::Shape::line(
                    vec![
                        center + egui::vec2(-7.0, 0.0),
                        center + egui::vec2(-2.0, 5.0),
                        center + egui::vec2(8.0, -6.0),
                    ],
                    stroke,
                ));
            } else {
                painter.circle_stroke(center, 12.0, Stroke::new(1.5_f32, color));
                painter.hline(
                    egui::Rangef::new(center.x - 4.5, center.x + 4.5),
                    center.y,
                    Stroke::new(1.5_f32, color),
                );
            }
            ui.vertical(|ui| {
                ui.label(theme::strong(title, 20.0));
                ui.add(egui::Label::new(RichText::new(label).size(14.0).color(color)).wrap());
                controls(ui);
            });
        });
    });
}

fn setup_card(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(theme::color(ui.ctx(), SURFACE))
        .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            content(ui);
        });
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
    theme::heading(ui, label, 15.0).id
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
        ui.label(theme::strong(title, 15.0).color(theme::color(ui.ctx(), color)));
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
    let fill = theme::color(ui.ctx(), LIME);
    ui.add_enabled(
        enabled,
        egui::Button::new(theme::strong(title, 15.0).color(theme::on_fill(fill)))
            .fill(fill)
            .min_size(egui::vec2(143.0, 44.0))
            .corner_radius(12),
    )
}

fn secondary(ui: &mut Ui, title: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(theme::strong(title, 14.0))
            .min_size(egui::vec2(78.0, 44.0))
            .corner_radius(12),
    )
}

fn destructive(ui: &mut Ui, title: &str, enabled: bool) -> egui::Response {
    let fill = destructive_fill(ui.ctx());
    ui.add_enabled(
        enabled,
        egui::Button::new(theme::strong(title, 15.0).color(theme::on_fill(fill)))
            .fill(fill)
            .min_size(egui::vec2(143.0, 44.0))
            .corner_radius(12),
    )
}

fn destructive_fill(ctx: &egui::Context) -> Color32 {
    theme::destructive_fill(ctx)
}

fn monitor_button(ui: &mut Ui, title: &str, enabled: bool, fill: Color32) -> egui::Response {
    let ctx = ui.ctx();
    let muted = fill == LINE;
    let fill = match fill {
        ERROR => destructive_fill(ctx),
        LINE if ctx.theme() == egui::Theme::Dark => theme::color(ctx, RAIL),
        _ => theme::color(ctx, fill),
    };
    let text = if muted {
        theme::on_fill(fill).gamma_multiply(0.78)
    } else {
        theme::on_fill(fill)
    };
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(title).size(14.0).strong().color(text))
            .fill(fill)
            .min_size(egui::vec2(0.0, 34.0))
            .corner_radius(10),
    )
}

fn connect_option(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    enabled: bool,
    background: Color32,
) -> bool {
    let fill = theme::color(ui.ctx(), if selected { LIME } else { background });
    ui.add_enabled(
        enabled,
        egui::Button::selectable(
            selected,
            RichText::new(label).strong().color(theme::on_fill(fill)),
        )
        .min_size(egui::vec2(0.0, 34.0))
        .fill(fill)
        .corner_radius(10),
    )
    .clicked()
}

fn connect_copy_row(ui: &mut Ui, label: &str, action: &str, value: &str) -> bool {
    field_label(ui, label);
    let display = Frame::new()
        .fill(theme::color(ui.ctx(), CREAM))
        .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
        .inner_margin(egui::Margin::symmetric(9, 5))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 108.0).max(40.0);
                let text = egui::Label::new(RichText::new(value).monospace().size(13.0)).truncate();
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 32.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| ui.add(text).on_hover_text(value),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    monitor_button(ui, action, true, SURFACE).clicked()
                })
                .inner
            })
            .inner
        })
        .inner;
    if display {
        ui.ctx().copy_text(value.to_owned());
    }
    display
}

fn connection_result(ui: &mut Ui, label: &str, message: &str, passed: bool) {
    let ink = theme::color(ui.ctx(), if passed { GREEN } else { CAUTION });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(label).strong().color(ink));
        caption(ui, message);
    });
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum JobActionGlyph {
    Start,
    Stop,
    Update,
}

#[cfg(windows)]
fn job_action_button(
    ui: &mut Ui,
    title: &str,
    enabled: bool,
    fill: Color32,
    glyph: JobActionGlyph,
) -> egui::Response {
    let ctx = ui.ctx();
    let muted = fill == LINE;
    let fill = match fill {
        ERROR => destructive_fill(ctx),
        LINE if ctx.theme() == egui::Theme::Dark => theme::color(ctx, RAIL),
        _ => theme::color(ctx, fill),
    };
    let ink = theme::on_fill(fill);
    let ink = if muted { ink.gamma_multiply(0.78) } else { ink };
    let mut label = egui::text::LayoutJob::default();
    label.append(
        title,
        17.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            color: ink,
            ..Default::default()
        },
    );
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(label)
            .fill(fill)
            .min_size(egui::vec2(0.0, 32.0))
            .corner_radius(9),
    );
    let center = response.rect.left_center() + egui::vec2(13.0, 0.0);
    let point = |x, y| center + egui::vec2(x, y);
    let stroke = Stroke::new(
        1.3_f32,
        if enabled {
            ink
        } else {
            ink.gamma_multiply(0.5)
        },
    );
    let path = match glyph {
        JobActionGlyph::Start => vec![
            point(-3.5, -5.0),
            point(5.0, 0.0),
            point(-3.5, 5.0),
            point(-3.5, -5.0),
        ],
        JobActionGlyph::Stop => vec![
            point(-4.0, -4.0),
            point(4.0, -4.0),
            point(4.0, 4.0),
            point(-4.0, 4.0),
            point(-4.0, -4.0),
        ],
        JobActionGlyph::Update => {
            ui.painter().add(egui::Shape::line(
                vec![point(0.5, -5.5), point(4.5, -4.5), point(4.5, -0.5)],
                stroke,
            ));
            vec![
                point(2.5, -4.5),
                point(-2.5, -4.5),
                point(-4.8, -1.0),
                point(-3.8, 2.8),
                point(-0.5, 5.0),
                point(2.8, 4.2),
                point(4.8, 1.5),
            ]
        }
    };
    ui.painter().add(egui::Shape::line(path, stroke));
    response
}

const MONITOR_MIN_SIZE: Vec2 = Vec2::new(408.0, 360.0);
const COMPACT_MONITOR_WIDTH: f32 = 430.0;

fn compact_monitor_spacing(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.x = 5.0;
    ui.spacing_mut().button_padding.x = 8.0;
}

fn monitor_header<R>(ui: &mut Ui, actions: impl FnOnce(&mut Ui) -> R) -> R {
    let compact = ui.available_width() < COMPACT_MONITOR_WIDTH;
    ui.horizontal(|ui| {
        if compact {
            compact_monitor_spacing(ui);
        }
        monitor_mark(ui);
        ui.label(
            theme::strong("Render Bridge", if compact { 17.0 } else { 19.0 })
                .color(theme::on_fill(theme::color(ui.ctx(), RAIL))),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions)
            .inner
    })
    .inner
}

fn monitor_mark(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 27.0), egui::Sense::hover());
    for (x, y, tone) in [
        (1.0, 13.0, MARK_SHADE),
        (9.0, 7.0, RAIL_ACTIVE),
        (17.0, 1.0, LIME),
    ] {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min + egui::vec2(x, y), egui::Vec2::splat(14.0)),
            4,
            theme::color(ui.ctx(), tone),
        );
    }
}

#[cfg(windows)]
fn server_state(ui: &mut Ui, online: bool, owned: bool, compact: bool) {
    let color = if online { GREEN } else { MUTED };
    let label = match (online, compact) {
        (true, true) => "Online",
        (false, true) => "Offline",
        (true, false) => "Server online",
        (false, false) => "Server offline",
    };
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(8.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.0, theme::color(ui.ctx(), color));
    ui.label(theme::strong(label, 13.0))
        .on_hover_text(match (online, owned) {
            (true, false) => "Render Bridge is running outside this panel.",
            (true, true) => "Render Bridge is running.",
            _ => "Render Bridge is offline.",
        });
}

fn gaming_mode_button(ui: &mut Ui, on: bool, enabled: bool) -> egui::Response {
    let ctx = ui.ctx();
    let (label, fill, ink, border, hint) = if on {
        let fill = theme::color(ctx, GREEN);
        (
            "Gaming Mode · On",
            fill,
            theme::on_fill(fill),
            fill,
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
        concat!(
            "render_bridge_models:\n",
            "    base_path: {path}\n",
            "    checkpoints: checkpoints/\n",
            "    diffusion_models: diffusion_models/\n",
            "    text_encoders: text_encoders/\n",
            "    vae: vae/\n",
            "    loras: loras/\n",
            "    clip_vision: clip_vision/\n",
            "    background_removal: background_removal/\n",
            "    geometry_estimation: geometry_estimation/\n",
        ),
        path = path,
    )
}

fn copy_agent_prompt(ui: &mut Ui, installed: &Installed, enabled: bool) -> Option<&'static str> {
    if primary(ui, "Copy setup prompt", enabled).clicked() {
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
         Agent guide: {AGENT_GUIDE}",
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
    let first = args.next();
    if first
        .as_deref()
        .is_none_or(|value| value != "--job-monitor")
    {
        let mut setup = SetupLaunch::default();
        let mut options = first.into_iter().chain(args);
        while let Some(flag) = options.next() {
            let field = match flag.to_str() {
                Some("--config") if setup.config.is_none() => &mut setup.config,
                Some("--data-dir") if setup.directory.is_none() => &mut setup.directory,
                _ => {
                    setup.invalid = true;
                    break;
                }
            };
            let Some(value) = options
                .next()
                .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
            else {
                setup.invalid = true;
                break;
            };
            *field = Some(PathBuf::from(value));
        }
        if setup
            .directory
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            setup.invalid = true;
            setup.directory = None;
        }
        return LaunchMode::Setup(setup);
    }
    let flag = args.next();
    let value = args.next();
    let option = args.next();
    let start_server = cfg!(windows)
        && flag.as_deref().is_some_and(|value| value == "--data-dir")
        && option
            .as_deref()
            .is_some_and(|value| value == "--start-bridge-in-tray");
    let start_hidden = start_server
        || option
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
        start_server: start_server
            && matches!(&target, MonitorTarget::Installation(data) if !setup::attach_only(data)),
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
    let (flag, value, comfy_url) = match mode {
        LaunchMode::Controls {
            target: MonitorTarget::ComfyUrl(url),
            ..
        } => ("--comfy-url", OsString::from(url), Some(url.clone())),
        LaunchMode::Controls {
            target: MonitorTarget::Installation(directory),
            ..
        } => ("--data-dir", directory.as_os_str().to_owned(), None),
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
        comfy_url,
        open: false,
        selected: SettingsTab::default(),
        enabled: None,
        error: None,
        appearance_error: None,
        message: None,
        theme_editor: theme_ui::ThemeEditor::default(),
        assets: asset_ui::AssetEditor::default(),
        model_destination: None,
        model_destination_changed: false,
        diagnostics: SettingsDiagnostics::default(),
    })
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn model_paths_include_the_categories_used_by_animation_and_3d() {
        let paths = model_paths(Path::new("Models"));
        for category in ["clip_vision", "background_removal", "geometry_estimation"] {
            assert!(paths.contains(&format!("    {category}: {category}/\n")));
        }
    }

    #[cfg(windows)]
    #[test]
    fn jobs_header_actions_fit_the_minimum_window_width() {
        let ctx = egui::Context::default();
        theme::configure_for(&ctx, egui::ThemePreference::Dark, None);
        for (width, close) in [
            (MONITOR_MIN_SIZE.x, "Close"),
            (MONITOR_MIN_SIZE.x, "Hide"),
            (480.0, "Hide"),
        ] {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, MONITOR_MIN_SIZE.y),
                )),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(1.25);
            let _ = ctx.run(input, |ctx| {
                egui::TopBottomPanel::top("test-controls-banner")
                    .frame(Frame::new().inner_margin(egui::Margin::symmetric(14, 6)))
                    .show(ctx, |ui| {
                        let available = ui.available_rect_before_wrap();
                        let (left, buttons) = monitor_header(ui, |ui| {
                            let left = ui.max_rect().left();
                            let close = monitor_button(ui, close, true, RAIL);
                            let settings = monitor_button(ui, "Settings", true, RAIL);
                            let update = job_action_button(
                                ui,
                                "Update",
                                true,
                                SURFACE,
                                JobActionGlyph::Update,
                            );
                            (left, [update.rect, settings.rect, close.rect])
                        });
                        for button in buttons {
                            assert!(
                                button.left() >= left - 0.5,
                                "{button:?} crosses the title edge {left}"
                            );
                            assert!(button.right() <= available.right() + 0.5);
                        }
                    });
            });
        }
    }

    #[cfg(windows)]
    #[test]
    fn settings_window_prefers_roomy_view_and_fits_smaller_logical_displays() {
        assert_eq!(settings_window_size(None), egui::vec2(660.0, 560.0));
        assert_eq!(
            settings_window_size(Some(egui::vec2(1920.0, 1080.0))),
            egui::vec2(660.0, 560.0)
        );
        assert_eq!(
            settings_window_size(Some(egui::vec2(500.0, 432.0))),
            egui::vec2(468.0, 368.0)
        );
    }

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

    #[test]
    fn connection_verification_expires_when_the_saved_backend_or_folders_change() {
        let data = tempfile::tempdir().unwrap();
        let app = data.path().join("app");
        let python = setup::venv_python(&app);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(python, []).unwrap();
        for (file, _) in setup::RUNTIME_FILES {
            std::fs::write(app.join(file), []).unwrap();
        }
        for dir in ["input", "output", "other-output"] {
            std::fs::create_dir(data.path().join(dir)).unwrap();
        }
        let mut settings = serde_json::json!({
            "COMFY_ROOT": data.path(), "COMFY_URL": "http://127.0.0.1:8188"
        });
        let config = data.path().join("settings.json");
        std::fs::write(&config, settings.to_string()).unwrap();
        let installed = setup::existing_install_at(data.path()).unwrap();
        let target = VerificationTarget::capture(&installed);
        assert!(target.is_current());
        settings["COMFY_OUTPUT_DIR"] = serde_json::json!(data.path().join("other-output"));
        std::fs::write(&config, settings.to_string()).unwrap();
        assert!(!target.is_current());
    }

    #[test]
    fn installer_accepts_a_config_and_distinguishes_an_invalid_command() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("Render Bridge fixture");
        let mode = launch_mode(
            [
                OsString::from("--config"),
                OsString::from("profile.json"),
                OsString::from("--data-dir"),
                directory.clone().into_os_string(),
            ]
            .into_iter(),
        );
        assert!(matches!(mode, LaunchMode::Setup(SetupLaunch {
            config: Some(config), directory: Some(data), invalid: false,
        }) if config == Path::new("profile.json") && data == directory));
        let invalid = launch_mode([OsString::from("--config")].into_iter());
        assert!(matches!(
            invalid,
            LaunchMode::Setup(SetupLaunch { invalid: true, .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn setup_starts_the_selected_installation_in_the_tray() {
        let folder = std::env::current_dir()
            .unwrap()
            .join("Render Bridge fixture");
        let arguments = [
            OsString::from("--job-monitor"),
            OsString::from("--data-dir"),
            folder.clone().into_os_string(),
            OsString::from("--start-bridge-in-tray"),
        ];
        assert!(matches!(
            launch_mode(arguments.into_iter()),
            LaunchMode::Controls {
                target: MonitorTarget::Installation(path),
                start_hidden: true,
                start_server: true,
            } if path == folder
        ));
        assert!(matches!(
            launch_mode(
                [
                    "--job-monitor",
                    "--comfy-url",
                    "http://127.0.0.1:8188",
                    "--start-bridge-in-tray"
                ]
                .into_iter()
                .map(OsString::from)
            ),
            LaunchMode::Controls {
                target: MonitorTarget::Missing,
                start_server: false,
                ..
            }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn attach_only_monitor_starts_hidden_without_starting_a_bridge() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path();
        let arguments = [
            OsString::from("--job-monitor"),
            OsString::from("--data-dir"),
            folder.as_os_str().to_owned(),
            OsString::from("--start-bridge-in-tray"),
        ];
        assert!(matches!(
            launch_mode(arguments.clone().into_iter()),
            LaunchMode::Controls {
                start_server: true,
                ..
            }
        ));
        std::fs::write(folder.join("attach-only"), "").unwrap();
        let mode = launch_mode(arguments.into_iter());
        assert!(matches!(
            &mode,
            LaunchMode::Controls {
                target: MonitorTarget::Installation(path), start_hidden: true, start_server: false
            } if path == folder
        ));
        let setting = startup_setting(&mode).unwrap();
        assert_eq!(setting.arguments.last().unwrap(), "--start-in-tray");
        assert!(matches!(
            launch_mode(setting.arguments.into_iter()),
            LaunchMode::Controls {
                start_hidden: true,
                start_server: false,
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
            LaunchMode::Controls { target: MonitorTarget::Installation(path), start_hidden: true, start_server: false } if path == &folder
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
                    start_server: false,
                }
            }
            _ => launch_mode(args),
        }
    };
    #[cfg(not(windows))]
    let mode = launch_mode(args);
    let start_server = matches!(
        &mode,
        LaunchMode::Controls {
            start_server: true,
            ..
        }
    );
    let (title, size, minimum, start_hidden) = match &mode {
        LaunchMode::Setup(_) => (
            "Render Bridge Installer",
            Vec2::new(920.0, 650.0),
            Vec2::new(720.0, 520.0),
            false,
        ),
        LaunchMode::Controls { start_hidden, .. } => (
            "Render Bridge",
            Vec2::new(480.0, 365.0),
            MONITOR_MIN_SIZE,
            cfg!(windows) && *start_hidden,
        ),
    };
    #[cfg(windows)]
    let key = tray_key(&mode);
    #[cfg(windows)]
    let startup_setting = startup_setting(&mode);
    #[cfg(windows)]
    if key.as_deref().is_some_and(|key| {
        if start_server {
            tray::start_existing(key)
        } else {
            tray::activate_existing(key, !start_hidden)
        }
    }) {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icons::window_icon())
            .with_inner_size(size)
            .with_min_inner_size(minimum)
            .with_visible(!start_hidden)
            .with_active(!start_hidden && !start_server)
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
                        .and_then(|controls| controls.data_dir.as_deref())
                        .is_some_and(|data| !setup::attach_only(data));
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
                if start_server {
                    installer.load_controls(&creation.egui_ctx);
                    installer.start_panel_server(&creation.egui_ctx, true);
                }
                installer
            };
            Ok(Box::new(installer))
        }),
    )
}

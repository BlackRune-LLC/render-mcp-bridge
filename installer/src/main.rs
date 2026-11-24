#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod connection;
mod discovery;
mod setup;
mod shortcuts;

use connection::{Client, RunningBridge};
use eframe::egui::{self, Color32, Frame, RichText, Stroke, Ui, Vec2};
use setup::{Installed, ModelCheck, Settings};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

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
const CAUTION: Color32 = Color32::from_rgb(118, 72, 25);
const ERROR: Color32 = Color32::from_rgb(133, 46, 40);

const COMFY_GUIDE: &str = "https://docs.comfy.org/installation/manual_install";
const MODEL_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge/blob/main/docs/pipelines.md";
const CLIENT_GUIDE: &str =
    "https://github.com/BlackRune-LLC/render-mcp-bridge#connect-an-mcp-client";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Comfy,
    Models,
    Install,
    Connect,
}

const STEPS: [(Step, &str, &str); 4] = [
    (Step::Comfy, "01", "ComfyUI"),
    (Step::Models, "02", "Models"),
    (Step::Install, "03", "Install"),
    (Step::Connect, "04", "Connect"),
];

enum Event {
    Checked(CheckOutcome),
    Discovered(Vec<discovery::Instance>),
    Verified(Option<RunningBridge>, Result<serde_json::Value, String>),
    Progress(String),
    Finished(Result<Installed, String>),
}

enum CheckOutcome {
    Models(Vec<ModelCheck>, bool),
    Offline(String),
    Failed(String),
}

struct Installer {
    step: Step,
    settings: Settings,
    check: Option<CheckOutcome>,
    worker: Option<Receiver<Event>>,
    progress: String,
    install_error: Option<String>,
    installed: Option<Installed>,
    copied: Option<&'static str>,
    instances: Vec<discovery::Instance>,
    searching: bool,
    discovery_done: bool,
    manual_confirmed: bool,
    gaming_shortcut: bool,
    shortcut_message: Option<(String, bool)>,
    running: Option<RunningBridge>,
    verification: Option<Result<serde_json::Value, String>>,
    verify_claude: bool,
    verify_codex: bool,
    codex_profile: String,
}

impl Installer {
    fn new(ctx: &egui::Context) -> Self {
        ctx.set_theme(egui::Theme::Light);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(13.0, 8.0);
            style.visuals.override_text_color = Some(INK);
            style.visuals.hyperlink_color = VIOLET;
            style.visuals.extreme_bg_color = SURFACE;
            style.visuals.selection.bg_fill = RAIL_ACTIVE;
            style.visuals.selection.stroke = Stroke::new(1.0_f32, VIOLET);
            style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, LINE);
            style.visuals.widgets.inactive.bg_fill = SURFACE;
            style.visuals.widgets.inactive.weak_bg_fill = SURFACE;
            style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, VIOLET);
            style.visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, VIOLET);
            for state in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                state.corner_radius = egui::CornerRadius::same(12);
            }
        });
        let mut installer = Self {
            step: Step::Comfy,
            settings: setup::default_settings(),
            check: None,
            worker: None,
            progress: String::new(),
            install_error: None,
            installed: None,
            copied: None,
            instances: Vec::new(),
            searching: false,
            discovery_done: false,
            manual_confirmed: false,
            gaming_shortcut: false,
            shortcut_message: None,
            running: None,
            verification: None,
            verify_claude: true,
            verify_codex: true,
            codex_profile: String::new(),
        };
        if setup::existing_install().is_none() {
            installer.start_discovery(ctx);
        }
        installer
    }

    fn change_step(&mut self, step: Step) {
        self.step = step;
        self.copied = None;
    }

    fn settings_changed(&mut self) {
        self.check = None;
        self.install_error = None;
        self.manual_confirmed = false;
    }

    fn can_continue(&self) -> bool {
        matches!(self.check, Some(CheckOutcome::Models(_, true)))
            || (self.manual_confirmed
                && matches!(
                    self.check,
                    Some(CheckOutcome::Models(_, false) | CheckOutcome::Offline(_))
                ))
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
                                Ok(verified) => CheckOutcome::Models(result.models, verified),
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

    fn start_install(&mut self, ctx: &egui::Context) {
        let settings = self.settings.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.install_error = None;
        self.progress = "Preparing installation…".into();
        match thread::Builder::new()
            .name("install-render-bridge".into())
            .spawn(move || {
                let result = setup::install(&settings, |message| {
                    let _ = sender.send(Event::Progress(message.into()));
                    ctx.request_repaint();
                });
                let _ = sender.send(Event::Finished(result));
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(receiver),
            Err(_) => self.install_error = Some("Could not start installation. Try again.".into()),
        }
    }

    fn receive_updates(&mut self) {
        while let Some(receiver) = &self.worker {
            match receiver.try_recv() {
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
                    self.verification = Some(result);
                }
                Ok(Event::Progress(message)) => self.progress = message,
                Ok(Event::Finished(result)) => {
                    self.worker = None;
                    match result {
                        Ok(installed) => {
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
                    self.searching = false;
                    let message = "Setup stopped unexpectedly. You can try this step again.".into();
                    if self.step == Step::Install {
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
                    .fill(RAIL)
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
                        RAIL_ACTIVE
                    } else {
                        Color32::TRANSPARENT
                    };
                    let foreground = if selected { INK } else { RAIL_TEXT };
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
            if let Some(installed) = setup::existing_install()
                && secondary(ui, "Open existing setup", idle).clicked()
            {
                self.installed = Some(installed);
                self.change_step(Step::Connect);
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
            ui.hyperlink_to("Install ComfyUI", COMFY_GUIDE);
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
                CheckOutcome::Models(_, true) => status(
                    ui,
                    "ComfyUI instance verified",
                    "The running API matched the selected input and output folders.",
                    GREEN,
                ),
                CheckOutcome::Models(_, false) => {
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
            Some(CheckOutcome::Models(models, _)) => {
                card(ui, |ui| {
                    for (index, model) in models.iter().enumerate() {
                        if index > 0 {
                            ui.separator();
                        }
                        ui.horizontal_top(|ui| {
                            let (label, color) = match model.available {
                                Some(true) => ("Found", GREEN),
                                Some(false) => ("Missing", CAUTION),
                                None => ("Unknown", MUTED),
                            };
                            ui.add_sized(
                                egui::vec2(70.0, 25.0),
                                egui::Label::new(
                                    RichText::new(label).size(14.0).strong().color(color),
                                ),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&model.filename).monospace().size(13.0),
                                )
                                .wrap(),
                            );
                        });
                    }
                });
            }
            _ if self.worker.is_some() => activity(ui, "Checking the model loaders…"),
            Some(CheckOutcome::Offline(message)) => {
                status(ui, "Model status unavailable", message, CAUTION)
            }
            Some(CheckOutcome::Failed(message)) => {
                status(ui, "Check your setup details", message, ERROR)
            }
            None => caption(
                ui,
                "Run the ComfyUI check to see which starter files are available.",
            ),
        }
        ui.add_space(6.0);
        caption(
            ui,
            "Missing models can be added later. Setup installs the bridge and its Python dependencies.",
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
        });
        ui.add_space(9.0);
        caption(
            ui,
            "Install uses Python 3.11 or newer to download the bridge dependencies. The next step shows how to connect your MCP client.",
        );
        if self.worker.is_some() {
            ui.add_space(9.0);
            activity(ui, &self.progress);
            caption(ui, "Keep this window open until installation finishes.");
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
            "Start the Bridge and check the saved MCP setup for the clients you use.",
        );
        let Some(installed) = &self.installed else {
            return;
        };
        let mut copied = None;
        let idle = self.worker.is_none();
        let mut selection_changed = false;
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
        ui.hyperlink_to("Configure Claude Code or Codex", CLIENT_GUIDE);
        if self.worker.is_some() {
            activity(ui, "Starting the Bridge and checking authenticated MCP…");
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
        caption(
            ui,
            "Stop Bridge or close this window to stop the Bridge started here.",
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
            }
        });
        if copied.is_some() {
            self.copied = copied;
        }
        if let Some(message) = self.copied {
            ui.label(RichText::new(message).size(14.0).color(GREEN));
        }
    }

    fn actions(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("setup-actions")
            .exact_height(82.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(CREAM)
                    .stroke(Stroke::new(1.0_f32, LINE))
                    .inner_margin(egui::Margin::symmetric(38, 17)),
            )
            .show(ctx, |ui| {
                let idle = self.worker.is_none();
                ui.horizontal(|ui| {
                    if self.step == Step::Connect && idle {
                        if self.running.is_some() {
                            if secondary(ui, "Stop Bridge", true).clicked() {
                                self.running = None;
                                self.verification = None;
                            }
                        } else if secondary(ui, "Close", true).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if self.running.is_none() && secondary(ui, "Change ComfyUI", true).clicked()
                        {
                            self.settings_changed();
                            self.change_step(Step::Comfy);
                        }
                    }
                    if matches!(self.step, Step::Models | Step::Install)
                        && secondary(ui, "Back", idle).clicked()
                    {
                        self.change_step(match self.step {
                            Step::Install => Step::Models,
                            _ => Step::Comfy,
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (label, ready) = match self.step {
                            Step::Comfy if self.can_continue() => ("Continue", idle),
                            Step::Comfy => (
                                "Check ComfyUI",
                                idle && !self.settings.comfy_root.trim().is_empty()
                                    && !self.settings.comfy_url.trim().is_empty(),
                            ),
                            Step::Models => ("Continue to install", self.can_continue() && idle),
                            Step::Install => ("Install Render Bridge", idle),
                            Step::Connect if self.running.is_some() => ("Verify again", idle),
                            Step::Connect => ("Start & Verify", idle),
                        };
                        if primary(ui, label, ready).clicked() {
                            match self.step {
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
        if matches!(self.step, Step::Install | Step::Connect)
            && self.worker.is_some()
            && ctx.input(|input| input.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.rail(ctx);
        self.actions(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(CREAM)
                    .inner_margin(egui::Margin::symmetric(38, 34)),
            )
            .show(ctx, |ui| {
                let step = self.step;
                egui::ScrollArea::vertical()
                    .id_salt(match step {
                        Step::Comfy => "comfy-page",
                        Step::Models => "models-page",
                        Step::Install => "install-page",
                        Step::Connect => "connect-page",
                    })
                    .auto_shrink([false, false])
                    .show(ui, |ui| match step {
                        Step::Comfy => self.comfy_screen(ui),
                        Step::Models => self.models_screen(ui),
                        Step::Install => self.install_screen(ui),
                        Step::Connect => self.connect_screen(ui),
                    });
            });
    }
}

fn bridge_status(code: &str) -> (&'static str, bool) {
    match code {
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
    ui.add(egui::Label::new(RichText::new(description).size(17.0).color(MUTED)).wrap());
    ui.add_space(19.0);
}

fn bridge_mark(ui: &mut Ui) {
    let (bounds, _) = ui.allocate_exact_size(egui::vec2(47.0, 44.0), egui::Sense::hover());
    for (x, y, color) in [
        (2.0, 23.0, Color32::from_rgb(180, 151, 237)),
        (13.0, 13.0, RAIL_ACTIVE),
        (24.0, 3.0, LIME),
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
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, LINE))
        .corner_radius(16)
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
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
    ui.add(egui::Label::new(RichText::new(message).size(14.0).color(MUTED)).wrap());
}

fn status(ui: &mut Ui, title: &str, detail: &str, color: Color32) {
    ui.add_space(5.0);
    card(ui, |ui| {
        ui.label(RichText::new(title).size(15.0).strong().color(color));
        caption(ui, detail);
    });
}

fn activity(ui: &mut Ui, message: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Spinner::new().size(18.0).color(VIOLET));
        ui.label(RichText::new(message).size(15.0).color(MUTED));
    });
}

fn primary(ui: &mut Ui, title: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(title).size(15.0).strong().color(INK))
            .fill(LIME)
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

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(Vec2::new(990.0, 670.0))
            .with_min_inner_size(Vec2::new(760.0, 540.0))
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        "Render Bridge Installer",
        options,
        Box::new(|creation| Ok(Box::new(Installer::new(&creation.egui_ctx)))),
    )
}

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod setup;

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
const MARK_TILE_SIZE: u8 = 19;
const MARK_TILE_RADIUS: u8 = 7;
const MARK_TILES: [(f32, f32, Color32); 3] = [
    (2.0, 23.0, Color32::from_rgb(180, 151, 237)),
    (13.0, 13.0, RAIL_ACTIVE),
    (24.0, 3.0, LIME),
];

const COMFY_GUIDE: &str = "https://docs.comfy.org/installation/manual_install";
const COMFY_DESKTOP: &str = "https://comfy.org/download";
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
    Progress(String),
    Finished(Result<Installed, String>),
}

enum CheckOutcome {
    Models(Vec<ModelCheck>),
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
        Self {
            step: Step::Comfy,
            settings: setup::default_settings(),
            check: None,
            worker: None,
            progress: String::new(),
            install_error: None,
            installed: None,
            copied: None,
        }
    }

    fn change_step(&mut self, step: Step) {
        self.step = step;
        self.copied = None;
    }

    fn settings_changed(&mut self) {
        self.check = None;
        self.install_error = None;
    }

    fn can_continue(&self) -> bool {
        matches!(
            &self.check,
            Some(CheckOutcome::Models(_) | CheckOutcome::Offline(_))
        )
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        let settings = self.settings.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.check = None;
        match thread::Builder::new()
            .name("check-comfyui".into())
            .spawn(move || {
                let result = match setup::validate_settings(&settings) {
                    Ok(settings) => match setup::probe(&settings) {
                        Ok(models) => CheckOutcome::Models(models),
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
                Ok(Event::Progress(message)) => self.progress = message,
                Ok(Event::Finished(result)) => {
                    self.worker = None;
                    match result {
                        Ok(installed) => {
                            self.installed = Some(installed);
                            self.change_step(Step::Connect);
                        }
                        Err(message) => self.install_error = Some(message),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    let message = "Setup stopped unexpectedly. You can try this step again.".into();
                    if self.step == Step::Install {
                        self.install_error = Some(message);
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
            "Choose the folder where ComfyUI saves files, then check its local API.",
        );
        let idle = self.worker.is_none();
        let mut changed = false;
        card(ui, |ui| {
            ui.add_enabled_ui(idle, |ui| {
                let root_label = field_label(ui, "ComfyUI folder");
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 108.0).max(150.0);
                    changed |= text_edit(ui, &mut self.settings.comfy_root, width, root_label);
                    if secondary(ui, "Browse…", true).clicked() {
                        let mut dialog =
                            rfd::FileDialog::new().set_title("Choose the ComfyUI folder");
                        let current = Path::new(&self.settings.comfy_root);
                        if current.is_dir() {
                            dialog = dialog.set_directory(current);
                        }
                        if let Some(path) = dialog.pick_folder() {
                            self.settings.comfy_root = path.to_string_lossy().into_owned();
                            changed = true;
                        }
                    }
                });
                caption(ui, "Select the folder that contains input and output.");
                ui.add_space(9.0);
                let url_label = field_label(ui, "Local API address");
                let url_width = ui.available_width();
                changed |= text_edit(ui, &mut self.settings.comfy_url, url_width, url_label);
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
            ui.hyperlink_to("ComfyUI manual setup guide", COMFY_GUIDE);
            ui.hyperlink_to("Get Comfy Desktop from Comfy Org", COMFY_DESKTOP);
        });
        if self.worker.is_some() {
            activity(ui, "Checking the local API and model loaders…");
        }
        if let Some(result) = &self.check {
            match result {
                CheckOutcome::Models(_) => status(
                    ui,
                    "ComfyUI responded",
                    "Continue to see which starter files it found.",
                    GREEN,
                ),
                CheckOutcome::Offline(message) => {
                    status(ui, "Could not verify ComfyUI", message, CAUTION);
                    caption(
                        ui,
                        "You can continue setup while ComfyUI is offline. Start it before using the tools.",
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
            Some(CheckOutcome::Models(models)) => {
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

    fn install_screen(&self, ui: &mut Ui) {
        header(
            ui,
            "Install Render Bridge",
            "Install the bridge for your user account with a dedicated Python environment.",
        );
        card(ui, |ui| {
            field_label(ui, "ComfyUI folder");
            code_label(ui, &self.settings.comfy_root);
            ui.separator();
            field_label(ui, "Local API address");
            code_label(ui, &self.settings.comfy_url);
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
        if let Some(message) = &self.install_error {
            status(ui, "Installation could not finish", message, ERROR);
        }
    }

    fn connect_screen(&mut self, ui: &mut Ui) {
        header(
            ui,
            "Connect your MCP client",
            "Run the start command in a terminal. Use the endpoint and private token file to register Render Bridge.",
        );
        let Some(installed) = &self.installed else {
            return;
        };
        let mut copied = None;
        card(ui, |ui| {
            field_label(ui, "Installation folder");
            code_label(ui, &installed.app_dir.to_string_lossy());
            ui.separator();
            if copy_row(
                ui,
                "Start command",
                "Copy command",
                &installed.start_command,
            ) {
                copied = Some("Start command copied");
            }
            ui.separator();
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
        if copied.is_some() {
            self.copied = copied;
        }
        if let Some(message) = self.copied {
            ui.label(RichText::new(message).size(14.0).color(GREEN));
        }
        caption(
            ui,
            "Start the server first if the token file doesn't exist. Enter the token only in your private client settings.",
        );
        ui.hyperlink_to("Open MCP client instructions", CLIENT_GUIDE);
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
                            Step::Connect => ("Done", true),
                        };
                        if primary(ui, label, ready).clicked() {
                            match self.step {
                                Step::Comfy if self.can_continue() => {
                                    self.change_step(Step::Models)
                                }
                                Step::Comfy => self.start_check(ctx),
                                Step::Models => self.change_step(Step::Install),
                                Step::Install => self.start_install(ctx),
                                Step::Connect => {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close)
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
        if self.step == Step::Install
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

fn header(ui: &mut Ui, title: &str, description: &str) {
    ui.add(egui::Label::new(RichText::new(title).size(36.0).strong()).wrap());
    ui.add(egui::Label::new(RichText::new(description).size(17.0).color(MUTED)).wrap());
    ui.add_space(19.0);
}

fn bridge_mark(ui: &mut Ui) {
    let (bounds, _) = ui.allocate_exact_size(egui::vec2(47.0, 44.0), egui::Sense::hover());
    let side = f32::from(MARK_TILE_SIZE);
    for (x, y, color) in MARK_TILES {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(bounds.min + egui::vec2(x, y), egui::vec2(side, side)),
            MARK_TILE_RADIUS,
            color,
        );
    }
}

fn inside_rounded_square(point: Vec2, side: f32, radius: f32) -> bool {
    let half = 0.5 * side;
    let straight_edge = half - radius;
    let dx = ((point.x - half).abs() - straight_edge).max(0.0);
    let dy = ((point.y - half).abs() - straight_edge).max(0.0);
    dx * dx + dy * dy <= radius * radius
}

fn installer_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let offsets = [
        egui::vec2(0.25, 0.25),
        egui::vec2(0.75, 0.25),
        egui::vec2(0.25, 0.75),
        egui::vec2(0.75, 0.75),
    ];
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Four samples smooth the rounded edges when the OS scales down the icon.
            let mut sum = [0_u16; 3];
            let mut covered = 0_u16;
            for offset in offsets {
                let point = egui::vec2(x as f32, y as f32) + offset;
                if !inside_rounded_square(point, SIZE as f32, 14.0) {
                    continue;
                }
                let mark = (point - egui::vec2(5.0, 5.0)) / 1.2;
                let color = MARK_TILES
                    .iter()
                    .rev()
                    .find(|(x, y, _)| {
                        inside_rounded_square(
                            mark - egui::vec2(*x, *y),
                            f32::from(MARK_TILE_SIZE),
                            f32::from(MARK_TILE_RADIUS),
                        )
                    })
                    .map_or(RAIL, |(_, _, color)| *color);
                for (channel, component) in sum.iter_mut().zip(color.to_array()) {
                    *channel += u16::from(component);
                }
                covered += 1;
            }
            if covered == 0 {
                rgba.extend_from_slice(&[0; 4]);
            } else {
                rgba.extend(sum.map(|channel| (channel / covered) as u8));
                rgba.push(((covered * 255 + 2) / 4) as u8);
            }
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
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

fn installer_viewport() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_inner_size(Vec2::new(990.0, 670.0))
        .with_min_inner_size(Vec2::new(760.0, 540.0))
        .with_resizable(true)
        .with_icon(installer_icon())
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: installer_viewport(),
        ..Default::default()
    };
    eframe::run_native(
        "Render Bridge Installer",
        options,
        Box::new(|creation| Ok(Box::new(Installer::new(&creation.egui_ctx)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_uses_an_explicit_render_bridge_icon() {
        let icon = installer_viewport().icon.expect("Render Bridge icon");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba.len(), (64 * 64 * 4) as usize);
        assert_eq!(&icon.rgba[..4], &[0; 4]);
        for color in [RAIL, MARK_TILES[0].2, RAIL_ACTIVE, LIME] {
            assert!(
                icon.rgba
                    .chunks_exact(4)
                    .any(|pixel| pixel == color.to_array())
            );
        }
    }
}

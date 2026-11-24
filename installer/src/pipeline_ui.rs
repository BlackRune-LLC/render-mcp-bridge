use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, Button, Color32, Frame, Margin, RichText, ScrollArea, Stroke, Ui};

use crate::pipeline::{self, Availability, Dependencies, Dependency, Revision, Variant};
use crate::{
    CAUTION, CREAM, GREEN, INK, LINE, MODEL_GUIDE, MUTED, RAIL_ACTIVE, SURFACE, VIOLET, activity,
    caption, code_label, primary, secondary, status, theme,
};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Node {
    Character,
    DrivingClip,
    #[default]
    MotionModel,
    Animate,
    Decode,
    Output,
}

impl Node {
    fn title(self) -> &'static str {
        match self {
            Self::Character => "Character",
            Self::DrivingClip => "Driving clip",
            Self::MotionModel => "Motion model",
            Self::Animate => "Animate",
            Self::Decode => "VAE decode",
            Self::Output => "Save video",
        }
    }

    fn summary(self, draft: Variant) -> &'static str {
        match self {
            Self::Character => "Image",
            Self::DrivingClip => "Video",
            Self::MotionModel => draft.label(),
            Self::Animate => "Motion transfer",
            Self::Decode => "Video frames",
            Self::Output => "MP4 + frames",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Character => "LoadImage reads the character reference from your agent.",
            Self::DrivingClip => "LoadVideo reads the driving clip from your agent.",
            Self::MotionModel => "UNETLoader uses the selected Wan Animate 2 weight.",
            Self::Animate => {
                "WanAnimate2ToVideo, BasicScheduler, and SamplerCustom transfer motion using the model variant’s sampler settings."
            }
            Self::Decode => {
                "VAEDecode produces video frames using the same Wan VAE for both variants."
            }
            Self::Output => "SaveVideo and SaveImage write the MP4 and its frames.",
        }
    }
}

#[derive(Eq, PartialEq)]
struct CheckTarget {
    backend: String,
    variant: Variant,
    generation: u64,
}

type CheckResult = Result<Dependencies, String>;

#[derive(Default)]
pub struct PipelinePanel {
    data_dir: Option<PathBuf>,
    latest: Option<Revision>,
    history_error: Option<String>,
    draft: Variant,
    node: Node,
    backend: Option<String>,
    validation: Option<CheckResult>,
    worker: Option<(CheckTarget, Receiver<CheckResult>)>,
    generation: u64,
    copied: bool,
    edit_workflow: bool,
    workflow: crate::workflow_ui::WorkflowPanel,
}

impl PipelinePanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: Option<&str>,
        data_dir: Option<&Path>,
        installed: Option<&crate::setup::Installed>,
    ) {
        self.refresh(data_dir, backend);
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.edit_workflow, false, "Built-in pipelines");
            ui.selectable_value(&mut self.edit_workflow, true, "Workflow editor");
        });
        if self.edit_workflow {
            self.workflow.show(ui, backend, installed);
            return;
        }
        ui.add_space(4.0);
        caption(ui, "Pipelines / Motion transfer");
        ui.horizontal_wrapped(|ui| {
            theme::heading(ui, "Wan Animate 2", 32.0);
            self.show_revision_actions(ui);
        });
        ui.add_space(5.0);
        let available = ui.available_width();
        if available >= 830.0 {
            let canvas = (available * 0.64).max(510.0);
            let inspector = available - canvas - ui.spacing().item_spacing.x;
            ui.horizontal_top(|ui| {
                pipeline_column(ui, canvas, |ui| {
                    self.graph(ui);
                    self.show_validation(ui);
                });
                pipeline_column(ui, inspector, |ui| {
                    self.swap_panel(ui);
                    self.show_revision_status(ui);
                });
            });
        } else {
            self.graph(ui);
            self.swap_panel(ui);
            self.show_validation(ui);
            self.show_revision_status(ui);
        }
    }

    fn refresh(&mut self, data_dir: Option<&Path>, backend: Option<&str>) {
        if self.data_dir.as_deref() != data_dir {
            self.data_dir = data_dir.map(Path::to_path_buf);
            self.latest = None;
            self.history_error = None;
            if let Some(data) = data_dir {
                match pipeline::load(data) {
                    Ok(history) => self.latest = history.last().copied(),
                    Err(error) => self.history_error = Some(error),
                }
            }
            self.draft = self
                .latest
                .map_or(Variant::Distilled, |revision| revision.variant);
            self.invalidate_validation();
        }
        if self.backend.as_deref() != backend {
            self.backend = backend.map(str::to_owned);
            self.invalidate_validation();
        }
        let Some((target, receiver)) = &self.worker else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Pipeline validation did not finish.".into()),
        };
        if self.backend.as_deref() == Some(target.backend.as_str())
            && self.draft == target.variant
            && self.generation == target.generation
        {
            self.validation = Some(result);
        }
        self.worker = None;
    }

    fn invalidate_validation(&mut self) {
        self.validation = None;
        self.copied = false;
        self.generation = self.generation.wrapping_add(1);
    }

    fn graph(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(theme::color(ui.ctx(), CREAM))
            .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
            .corner_radius(12)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ScrollArea::horizontal()
                    .id_salt("wan-graph")
                    .show(ui, |ui| {
                        let width = ui.available_width().max(490.0);
                        let height = 340.0;
                        let (canvas, _) =
                            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                        let painter = ui.painter_at(canvas);
                        let dot = theme::color(ui.ctx(), LINE).gamma_multiply(0.48);
                        for x in (12..width as usize).step_by(14) {
                            for y in (12..height as usize).step_by(14) {
                                painter.circle_filled(
                                    canvas.min + egui::vec2(x as f32, y as f32),
                                    0.8,
                                    dot,
                                );
                            }
                        }
                        let slot = (width - 8.0) / 5.0;
                        let node_rect = |column: u8, y: f32| {
                            egui::Rect::from_min_size(
                                canvas.min + egui::vec2(4.0 + f32::from(column) * slot, y),
                                egui::vec2(slot - 12.0, 57.0),
                            )
                        };
                        let image = node_rect(0, 76.0);
                        let clip = node_rect(0, 206.0);
                        let model = node_rect(1, 141.0);
                        let animate = node_rect(2, 141.0);
                        let decode = node_rect(3, 141.0);
                        let output = node_rect(4, 141.0);
                        let nodes = [
                            (Node::Character, image),
                            (Node::DrivingClip, clip),
                            (Node::MotionModel, model),
                            (Node::Animate, animate),
                            (Node::Decode, decode),
                            (Node::Output, output),
                        ];
                        let paths = [
                            (image.right_center(), animate.center_top(), -38.0),
                            (clip.right_center(), animate.center_bottom(), 38.0),
                            (model.right_center(), animate.left_center(), 0.0),
                            (animate.right_center(), decode.left_center(), 0.0),
                            (decode.right_center(), output.left_center(), 0.0),
                        ];
                        let normal_wire = theme::color(ui.ctx(), GREEN);
                        let motion_wire = if self.node == Node::MotionModel {
                            theme::color(ui.ctx(), VIOLET)
                        } else {
                            normal_wire
                        };
                        let wire = |start| {
                            if start == model.right_center() {
                                motion_wire
                            } else {
                                normal_wire
                            }
                        };
                        for (start, end, vertical_bend) in paths {
                            let reach = (end.x - start.x) * 0.5;
                            let approach = if vertical_bend == 0.0 {
                                egui::vec2(-reach, 0.0)
                            } else {
                                egui::vec2(0.0, vertical_bend)
                            };
                            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                                [start, start + egui::vec2(reach, 0.0), end + approach, end],
                                false,
                                Color32::TRANSPARENT,
                                Stroke::new(1.8_f32, wire(start)),
                            ));
                        }
                        for (node, rect) in nodes {
                            let response =
                                ui.interact(rect, ui.id().with(node.title()), egui::Sense::click());
                            if response.clicked() {
                                self.node = node;
                            }
                            let selected = self.node == node;
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    true,
                                    selected,
                                    node.title(),
                                )
                            });
                            let highlighted = selected || response.has_focus();
                            let fill = theme::color(
                                ui.ctx(),
                                if selected || response.hovered() {
                                    RAIL_ACTIVE
                                } else {
                                    SURFACE
                                },
                            );
                            let border =
                                theme::color(ui.ctx(), if highlighted { VIOLET } else { LINE });
                            painter.rect(
                                rect,
                                9,
                                fill,
                                Stroke::new(if highlighted { 2.0_f32 } else { 1.0_f32 }, border),
                                egui::StrokeKind::Inside,
                            );
                            let inner = rect.shrink2(egui::vec2(5.0, 3.0));
                            let label = painter.with_clip_rect(inner);
                            node_line(
                                &label,
                                inner,
                                -8.0,
                                node.title(),
                                theme::display_font(13.0),
                                theme::color(ui.ctx(), INK),
                            );
                            node_line(
                                &label,
                                inner,
                                10.0,
                                node.summary(self.draft),
                                egui::FontId::proportional(11.0),
                                theme::color(ui.ctx(), MUTED),
                            );
                            response.on_hover_text(node.description());
                        }
                        for (start, end, _) in paths {
                            let color = wire(start);
                            painter.circle_filled(start, 3.4, color);
                            painter.circle_filled(end, 3.4, color);
                        }
                    });
            });
    }

    fn swap_panel(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(theme::color(ui.ctx(), SURFACE))
            .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
            .corner_radius(12)
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                self.inspector(ui);
            });
    }

    fn inspector(&mut self, ui: &mut Ui) {
        theme::heading(
            ui,
            if self.node == Node::MotionModel {
                "Swap model"
            } else {
                self.node.title()
            },
            22.0,
        );
        caption(ui, self.node.description());
        if self.node != Node::MotionModel {
            if secondary(ui, "Select Motion model", true).clicked() {
                self.node = Node::MotionModel;
            }
            return;
        }
        let previous = self.draft;
        for candidate in [Variant::Distilled, Variant::FullQuality] {
            let selected = self.draft == candidate;
            let edge = theme::color(ui.ctx(), if selected { VIOLET } else { LINE });
            let fill = theme::color(ui.ctx(), if selected { RAIL_ACTIVE } else { CREAM });
            let button = Button::selectable(selected, theme::strong(candidate.label(), 15.0))
                .fill(fill)
                .stroke(Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, edge))
                .corner_radius(8);
            if ui.add_sized([ui.available_width(), 37.0], button).clicked() {
                self.draft = candidate;
            }
        }
        if self.draft != previous {
            self.invalidate_validation();
        }
        ui.separator();
        theme::heading(ui, "Review changes", 17.0);
        let baseline = self
            .latest
            .map_or(Variant::Distilled, |revision| revision.variant);
        if self.draft != baseline {
            caption(
                ui,
                &format!(
                    "Motion model: {} to {}",
                    baseline.label(),
                    self.draft.label()
                ),
            );
            caption(
                ui,
                &format!(
                    "Sampler: {} to {} LCM steps",
                    baseline.steps(),
                    self.draft.steps()
                ),
            );
            caption(
                ui,
                if self.draft.lora().is_some() {
                    "Adds required Lightx2v LoRA"
                } else {
                    "Removes the Lightx2v LoRA"
                },
            );
        } else {
            caption(ui, &format!("{} LCM sampler steps", self.draft.steps()));
        }
        ui.collapsing("Required files", |ui| {
            code_label(ui, self.draft.weight());
            if let Some(lora) = self.draft.lora() {
                code_label(ui, lora);
            }
        });
        caption(
            ui,
            "Text encoder, vision encoder, and VAE are shared by both variants.",
        );
    }

    fn show_validation(&self, ui: &mut Ui) {
        if self.worker.is_some() {
            activity(ui, "Checking model files and nodes…");
        }
        match &self.validation {
            Some(Ok(dependencies)) if dependencies.ready() => {
                status(
                    ui,
                    "Dependencies ready",
                    "The selected build reports the model files and nodes used by this clip workflow.",
                    GREEN,
                );
            }
            Some(Ok(dependencies)) => {
                status(
                    ui,
                    "Dependencies need attention",
                    "Refresh after installing the missing resources or confirming that the selected build reports them.",
                    CAUTION,
                );
                show_unready(ui, "Model files", &dependencies.files);
                show_unready(ui, "Nodes", &dependencies.nodes);
                ui.hyperlink_to("Model and pipeline guide", MODEL_GUIDE);
            }
            Some(Err(error)) => status(ui, "Could not verify dependencies", error, CAUTION),
            None if self.backend.is_none() => {
                caption(
                    ui,
                    "Select a local ComfyUI build to validate this pipeline.",
                );
            }
            None => {}
        }
    }

    fn show_revision_status(&self, ui: &mut Ui) {
        caption(
            ui,
            "Save a local revision. Copy agent options to use this variant in a new render.",
        );
        if let Some(revision) = self.latest {
            caption(
                ui,
                &format!(
                    "Saved revision {} · {}",
                    revision.number,
                    revision.variant.label()
                ),
            );
        }
        if let Some(error) = &self.history_error {
            status(ui, "Pipeline revision unavailable", error, CAUTION);
        }
        if self.copied {
            caption(
                ui,
                "Copied options for animate_from_clip. Your agent also needs the image, driving clip, and prompt.",
            );
        }
    }

    fn show_revision_actions(&mut self, ui: &mut Ui) {
        let ready = self
            .validation
            .as_ref()
            .is_some_and(|result| result.as_ref().is_ok_and(Dependencies::ready));
        let saved = self
            .latest
            .is_some_and(|revision| revision.variant == self.draft);
        if secondary(
            ui,
            "Validate",
            self.backend.is_some() && self.worker.is_none(),
        )
        .clicked()
        {
            self.start_validation(ui.ctx());
        }
        if primary(
            ui,
            "Save new revision",
            ready && !saved && self.data_dir.is_some(),
        )
        .clicked()
            && let Some(data) = &self.data_dir
        {
            match pipeline::append(data, self.draft) {
                Ok(number) => {
                    self.latest = Some(Revision {
                        number,
                        variant: self.draft,
                    });
                    self.history_error = None;
                }
                Err(error) => self.history_error = Some(error),
            }
        }
        if secondary(ui, "Copy agent options", ready).clicked() {
            ui.ctx().copy_text(self.draft.agent_args().to_owned());
            self.copied = true;
        }
    }

    fn start_validation(&mut self, ctx: &egui::Context) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let variant = self.draft;
        self.invalidate_validation();
        let target = CheckTarget {
            backend: backend.clone(),
            variant,
            generation: self.generation,
        };
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("render-bridge-pipeline-check".into())
            .spawn(move || {
                let _ = sender.send(pipeline::validate(&backend, variant));
                ctx.request_repaint();
            })
            .is_err()
        {
            self.validation = Some(Err("Could not start pipeline validation.".into()));
            return;
        }
        self.worker = Some((target, receiver));
    }
}

fn node_line(
    painter: &egui::Painter,
    rect: egui::Rect,
    offset: f32,
    text: &str,
    mut font: egui::FontId,
    color: Color32,
) {
    let mut galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    if galley.size().x > rect.width() {
        font.size *= rect.width() / galley.size().x;
        galley = painter.layout_no_wrap(text.to_owned(), font, color);
    }
    let center = rect.center() + egui::vec2(0.0, offset);
    painter.galley(center - galley.size() * 0.5, galley, color);
}

fn pipeline_column(ui: &mut Ui, width: f32, contents: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            contents(ui);
        },
    );
}

fn show_unready(ui: &mut Ui, label: &str, dependencies: &[Dependency]) {
    let unready = dependencies
        .iter()
        .filter(|item| item.availability != Availability::Ready);
    if unready.clone().next().is_none() {
        return;
    }
    ui.collapsing(label, |ui| {
        for dependency in unready {
            let (label, color) = match dependency.availability {
                Availability::Missing => ("Missing", CAUTION),
                Availability::Unknown => ("Unconfirmed", MUTED),
                Availability::Ready => ("Ready", GREEN),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(label).color(theme::color(ui.ctx(), color)));
                code_label(ui, dependency.name);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_validation_request_cannot_outlive_a_backend_switch() {
        let first = "http://127.0.0.1:8188";
        let mut panel = PipelinePanel::default();
        panel.refresh(None, Some(first));
        let (sender, receiver) = mpsc::channel();
        panel.worker = Some((
            CheckTarget {
                backend: first.into(),
                variant: panel.draft,
                generation: panel.generation,
            },
            receiver,
        ));
        panel.refresh(None, Some("http://127.0.0.1:8189"));
        panel.refresh(None, Some(first));
        sender.send(Err("old result".into())).unwrap();

        panel.refresh(None, Some(first));

        assert!(panel.validation.is_none());
        assert!(panel.worker.is_none());
    }
}

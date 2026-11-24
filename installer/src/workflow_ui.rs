use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, ComboBox, RichText, ScrollArea, TextEdit, Ui};
use serde_json::Value;

use crate::setup::Installed;
use crate::workflow_editor::{self, InputKind, InputSchema, WorkflowEditor, WorkflowSchema};
use crate::{CAUTION, GREEN, activity, caption, card, code_label, primary, secondary, status};

enum Reply {
    Inventory(Result<WorkflowSchema, String>),
    Test(Result<String, String>),
}

struct Worker {
    backend: String,
    test: bool,
    receiver: Receiver<Reply>,
}

#[derive(Default)]
struct PortForm {
    name: String,
    text: String,
    link: Option<(String, usize)>,
}

enum Edit {
    Literal(Value),
    Link(String, usize),
    Unset,
}

#[derive(Default)]
pub struct WorkflowPanel {
    graph: WorkflowEditor,
    path: Option<PathBuf>,
    node: Option<String>,
    form: PortForm,
    backend: Option<String>,
    schema: Option<WorkflowSchema>,
    worker: Option<Worker>,
    issues: Vec<String>,
    dependency_changes: (Vec<String>, Vec<String>),
    error: Option<String>,
    test_receipt: Option<(String, String)>,
    confirm_run: bool,
}

impl WorkflowPanel {
    pub fn show(&mut self, ui: &mut Ui, backend: Option<&str>, installed: Option<&Installed>) {
        if self.backend.as_deref() != backend {
            self.backend = backend.map(str::to_owned);
            if self.worker.as_ref().is_some_and(|worker| !worker.test) {
                self.worker = None;
            }
            self.schema = None;
            self.issues.clear();
            self.confirm_run = false;
            self.error = None;
        }
        self.poll();
        ui.heading("Workflow editor");
        ui.horizontal_wrapped(|ui| {
            if primary(ui, "Import API JSON", self.worker.is_none()).clicked() {
                self.import();
            }
            let loaded = self.path.is_some();
            if secondary(ui, "Export copy", loaded).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("ComfyUI API workflow", &["json"])
                    .set_file_name("workflow-edited.json")
                    .save_file()
            {
                self.error = self.graph.export(&path).err();
            }
            if secondary(ui, "Undo", self.graph.can_undo() && self.worker.is_none()).clicked() {
                self.graph.undo();
                self.sync_form();
                self.validate_draft();
                self.error = None;
            }
            if secondary(
                ui,
                "Check dependencies",
                loaded && backend.is_some() && self.worker.is_none(),
            )
            .clicked()
            {
                self.start(ui.ctx(), None);
            }
        });
        if let Some(error) = &self.error {
            status(ui, "Workflow action needs attention", error, CAUTION);
        }
        let Some(path) = &self.path else {
            caption(
                ui,
                "In ComfyUI, export a workflow in API Format, then open that JSON here to edit its inputs and connections.",
            );
            return;
        };
        if let Some(name) = path.file_name() {
            ui.label(RichText::new(name.to_string_lossy()).strong());
        }
        if self.worker.as_ref().is_some_and(|worker| !worker.test) {
            activity(ui, "Checking node types, ports, and model files…");
        }
        if backend.is_none() {
            caption(
                ui,
                "Select a local ComfyUI build to check compatible inputs and outputs.",
            );
        } else if self.schema.is_none() && self.worker.is_none() {
            caption(
                ui,
                "Check dependencies to load the choices reported by this build.",
            );
        }
        if ui.available_width() >= 700.0 {
            ui.columns(2, |columns| {
                self.graph_list(&mut columns[0]);
                self.inspector(&mut columns[1]);
            });
        } else {
            self.graph_list(ui);
            self.inspector(ui);
        }
        self.review(ui, installed);
    }

    fn import(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("ComfyUI API workflow", &["json"])
            .pick_file()
        else {
            return;
        };
        if self.graph.can_undo()
            && rfd::MessageDialog::new()
                .set_title("Open another workflow?")
                .set_description("Opening this file clears the current draft and undo history. Export a copy first if you want to keep your edits.")
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show()
                != rfd::MessageDialogResult::Ok
        {
            return;
        }
        match self.graph.import(&path) {
            Ok(()) => self.loaded(path),
            Err(error) => self.error = Some(error),
        }
    }

    fn loaded(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.node = self.graph.nodes().keys().next().cloned();
        self.form = PortForm::default();
        self.schema = None;
        self.issues.clear();
        self.dependency_changes = Default::default();
        self.error = None;
    }

    fn graph_list(&mut self, ui: &mut Ui) {
        let mut selected = self.node.clone();
        card(ui, |ui| {
            ui.label(RichText::new(format!("Nodes · {}", self.graph.nodes().len())).strong());
            ScrollArea::vertical()
                .id_salt("workflow-graph")
                .max_height(170.0)
                .show(ui, |ui| {
                    for (id, node) in self.graph.nodes() {
                        ui.selectable_value(
                            &mut selected,
                            Some(id.clone()),
                            format!("#{id}  {}", node.class_type),
                        );
                    }
                });
        });
        if self.node != selected {
            self.node = selected;
            self.form = PortForm::default();
            self.sync_form();
        }
    }

    fn inspector(&mut self, ui: &mut Ui) {
        let Some(id) = self.node.as_deref() else {
            return;
        };
        let Some(node) = self.graph.nodes().get(id) else {
            return;
        };
        let shape = self
            .schema
            .as_ref()
            .and_then(|schema| schema.nodes.get(&node.class_type));
        let mut names = node.inputs.keys().collect::<Vec<_>>();
        if let Some(shape) = shape {
            names.extend(
                shape
                    .inputs
                    .keys()
                    .filter(|name| !node.inputs.contains_key(*name)),
            );
            names.sort_unstable();
        }
        let mut input = self.form.name.clone();
        let mut edit = None;
        card(ui, |ui| {
            ui.label(RichText::new(format!("#{}  {}", id, node.class_type)).strong());
            if let Some(shape) = shape
                && !shape.outputs.is_empty()
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Outputs");
                    for (port, output) in shape.outputs.iter().enumerate() {
                        let name = &shape.output_names[port];
                        let label = if output == name {
                            format!("{port} · {output}")
                        } else {
                            format!("{port} · {name} ({output})")
                        };
                        code_label(ui, &label);
                    }
                });
            }
            if names.is_empty() {
                caption(ui, "No input ports reported.");
                return;
            }
            ComboBox::from_id_salt("workflow-input")
                .selected_text(if input.is_empty() {
                    "Choose an input"
                } else {
                    &input
                })
                .height(210.0)
                .show_ui(ui, |ui| {
                    for name in names {
                        let connected = link_value(node.inputs.get(name)).is_some();
                        let label = if connected {
                            format!("{name} · linked")
                        } else {
                            name.clone()
                        };
                        ui.selectable_value(&mut input, name.clone(), label);
                    }
                });
            if input != self.form.name || input.is_empty() {
                return;
            }
            let current = node.inputs.get(&input);
            if let Some(port) = shape.and_then(|shape| shape.inputs.get(&input)) {
                if self.worker.is_some() {
                    caption(
                        ui,
                        "Wait for the current workflow action to finish before editing.",
                    );
                    return;
                }
                caption(
                    ui,
                    if port.required {
                        "Required input"
                    } else {
                        "Optional input"
                    },
                );
                edit = port_editor(
                    ui,
                    &mut self.form,
                    current,
                    port,
                    id,
                    &self.graph,
                    self.schema.as_ref(),
                );
                if current.is_some()
                    && !port.required
                    && secondary(ui, "Clear optional input", true).clicked()
                {
                    edit = Some(Edit::Unset);
                }
            } else if let Some(current) = current {
                code_label(ui, &short_value(current));
            }
        });
        if input != self.form.name {
            self.form.name = input;
            self.sync_form();
        }
        if let Some(edit) = edit {
            self.apply_edit(edit);
        }
    }

    fn sync_form(&mut self) {
        let Some(node) = self.node.as_ref().and_then(|id| self.graph.nodes().get(id)) else {
            return;
        };
        let value = node.inputs.get(&self.form.name);
        self.form.link = link_value(value);
        self.form.text = match value {
            Some(Value::String(text)) => text.clone(),
            Some(value) if self.form.link.is_none() => value.to_string(),
            _ => String::new(),
        };
    }

    fn apply_edit(&mut self, edit: Edit) {
        let (Some(node), Some(schema)) = (&self.node, &self.schema) else {
            return;
        };
        let result = match edit {
            Edit::Unset => self.graph.unset(node, &self.form.name, schema),
            Edit::Literal(value) => self.graph.set_literal(node, &self.form.name, value, schema),
            Edit::Link(source, port) => {
                self.graph
                    .connect(node, &self.form.name, &source, port, schema)
            }
        };
        match result {
            Ok(_) => {
                self.error = None;
                self.sync_form();
                self.validate_draft();
            }
            Err(error) => {
                self.error = Some(error);
                self.sync_form();
            }
        }
    }

    fn validate_draft(&mut self) {
        if let Some(schema) = &self.schema {
            self.issues = self.graph.validate(schema);
            let changes = self.graph.dependency_changes();
            self.dependency_changes = (changes.added, changes.removed);
        }
    }

    fn review(&mut self, ui: &mut Ui, installed: Option<&Installed>) {
        if self.schema.is_some() {
            ui.separator();
            let (added, removed) = &self.dependency_changes;
            if !added.is_empty() || !removed.is_empty() {
                ui.collapsing("Model changes from the imported file", |ui| {
                    for file in added {
                        code_label(ui, &format!("Added: {file}"));
                    }
                    for file in removed {
                        code_label(ui, &format!("Removed: {file}"));
                    }
                });
            }
            if self.issues.is_empty() {
                status(
                    ui,
                    "Dependencies ready",
                    "The selected build reports the required nodes, choices, and compatible ports.",
                    GREEN,
                );
            } else {
                ui.collapsing(format!("{} dependency or port checks need attention", self.issues.len()), |ui| {
                    ScrollArea::vertical().id_salt("workflow-issues").max_height(130.0).show(ui, |ui| {
                        for issue in &self.issues {
                            ui.label(issue);
                        }
                    });
                    caption(ui, "Check Models for available packs. For other missing files or nodes, use the workflow author's instructions, then check dependencies again.");
                });
            }
        }
        let ready = self.schema.is_some()
            && self.issues.is_empty()
            && self.worker.is_none()
            && installed.and_then(crate::gaming_mode) == Some(false);
        if secondary(ui, "Test run…", ready)
            .on_disabled_hover_text(
                "Check dependencies on a local installation with Gaming Mode off.",
            )
            .clicked()
        {
            self.confirm_run = true;
        }
        if self.worker.as_ref().is_some_and(|worker| worker.test) {
            activity(ui, "Checking and submitting this workflow…");
        }
        if let Some((backend, id)) = &self.test_receipt {
            caption(
                ui,
                &format!("Test queued on {backend} · {id}. Watch queue activity in Jobs."),
            );
            if secondary(ui, "Copy job ID", true).clicked() {
                ui.ctx().copy_text(id.clone());
            }
        }
        if self.confirm_run {
            let response = crate::panel_modal(ui.ctx(), "workflow-confirm-test").show(ui.ctx(), |ui| {
                ui.heading("Queue a workflow test?");
                caption(ui, "The selected local build will run this graph and can write new outputs. Tests require built-in local nodes. Only run a workflow you trust.");
                if let Some(backend) = &self.backend {
                    code_label(ui, backend);
                }
                ui.horizontal_wrapped(|ui| {
                    if secondary(ui, "Cancel", true).clicked() {
                        self.confirm_run = false;
                    }
                    if primary(ui, "Queue test", ready).clicked() {
                        self.confirm_run = false;
                        self.start(ui.ctx(), installed.cloned());
                    }
                });
            });
            if response.should_close() {
                self.confirm_run = false;
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, installed: Option<Installed>) {
        let test = installed.is_some();
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let target = backend.clone();
        let graph = self.graph.clone();
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        if !test {
            self.schema = None;
            self.issues.clear();
        }
        match thread::Builder::new()
            .name("render-bridge-workflow".into())
            .spawn(move || {
                let answer = if let Some(installed) = installed {
                    Reply::Test(workflow_editor::submit_test(&target, &graph, || {
                        crate::setup::existing_install_at(&installed.data_dir).is_some_and(
                            |current| {
                                crate::gaming_mode(&current) == Some(false)
                                    && crate::setup::installed_comfy_url(&current).as_deref()
                                        == Some(target.as_str())
                            },
                        )
                    }))
                } else {
                    Reply::Inventory(workflow_editor::inspect(&target, &graph))
                };
                let _ = sender.send(answer);
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.error = None;
                self.worker = Some(Worker {
                    backend,
                    test,
                    receiver,
                });
            }
            Err(_) => self.error = Some("Could not start the workflow action.".into()),
        }
    }

    fn poll(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        let answer = match worker.receiver.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.worker = None;
                self.error = Some("The workflow action did not finish.".into());
                return;
            }
        };
        let backend = worker.backend.clone();
        self.worker = None;
        match answer {
            Reply::Inventory(result) if self.backend.as_deref() == Some(backend.as_str()) => {
                match result {
                    Ok(schema) => {
                        self.schema = Some(schema);
                        self.validate_draft();
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Reply::Inventory(_) => {}
            Reply::Test(Ok(id)) => self.test_receipt = Some((backend, id)),
            Reply::Test(Err(error)) => self.error = Some(format!("Test on {backend}: {error}")),
        }
    }
}

fn link_value(value: Option<&Value>) -> Option<(String, usize)> {
    let (source, port) = workflow_editor::link_parts(value?)?;
    Some((source.to_owned(), port))
}

fn short_value(value: &Value) -> String {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    let mut chars = text.chars();
    let mut short = chars.by_ref().take(160).collect::<String>();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

fn port_editor(
    ui: &mut Ui,
    form: &mut PortForm,
    current: Option<&Value>,
    port: &InputSchema,
    target: &str,
    graph: &WorkflowEditor,
    schema: Option<&WorkflowSchema>,
) -> Option<Edit> {
    if let Some((node, output)) = link_value(current) {
        caption(ui, &format!("Connected to #{node} output {output}"));
    }
    let value = match &port.kind {
        InputKind::Choice(choices) => {
            let mut selected = current.cloned();
            ComboBox::from_id_salt("workflow-literal-choice")
                .selected_text(current.map_or_else(|| "Choose a value".into(), short_value))
                .height(240.0)
                .show_ui(ui, |ui| {
                    for choice in choices {
                        ui.selectable_value(
                            &mut selected,
                            Some(choice.clone()),
                            short_value(choice),
                        );
                    }
                });
            selected.filter(|selected| Some(selected) != current)
        }
        InputKind::Boolean => {
            ui.horizontal(|ui| {
                let current = current.and_then(Value::as_bool);
                let off = ui
                    .selectable_label(current == Some(false), "False")
                    .clicked();
                let on = ui.selectable_label(current == Some(true), "True").clicked();
                if off && current != Some(false) {
                    Some(Value::Bool(false))
                } else if on && current != Some(true) {
                    Some(Value::Bool(true))
                } else {
                    None
                }
            })
            .inner
        }
        InputKind::Integer | InputKind::Float | InputKind::String => {
            if matches!(port.kind, InputKind::String) {
                ui.add(
                    TextEdit::multiline(&mut form.text)
                        .desired_rows(2)
                        .desired_width(330.0),
                );
            } else {
                ui.add(TextEdit::singleline(&mut form.text).desired_width(180.0));
            }
            let value = if matches!(port.kind, InputKind::String) {
                Some(Value::String(form.text.clone()))
            } else {
                let value = serde_json::from_str::<Value>(&form.text).ok();
                value.filter(|value| match port.kind {
                    InputKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
                    _ => value.is_number(),
                })
            };
            let changed = value.as_ref().is_some_and(|value| Some(value) != current);
            if secondary(ui, "Apply value", changed).clicked() {
                value
            } else {
                None
            }
        }
        InputKind::Link(_) | InputKind::Unknown => None,
    };
    if let Some(value) = value {
        return Some(Edit::Literal(value));
    }
    let schema = schema?;
    if matches!(port.kind, InputKind::Choice(_) | InputKind::Unknown) {
        return None;
    }
    link_editor(ui, form, current, target, graph, schema, port)
}

fn link_editor(
    ui: &mut Ui,
    form: &mut PortForm,
    current: Option<&Value>,
    target: &str,
    graph: &WorkflowEditor,
    schema: &WorkflowSchema,
    port: &InputSchema,
) -> Option<Edit> {
    let sources = graph
        .nodes()
        .iter()
        .filter(|(id, _)| id.as_str() != target)
        .filter_map(|(id, node)| Some((id, &node.class_type, schema.nodes.get(&node.class_type)?)))
        .flat_map(|(id, class, shape)| {
            shape
                .outputs
                .iter()
                .enumerate()
                .map(move |(index, kind)| (id, class, index, kind))
        })
        .filter(|(_, _, _, kind)| port.accepts_output(kind));
    ComboBox::from_id_salt("workflow-link")
        .selected_text(form.link.as_ref().map_or_else(
            || "Connect an output".into(),
            |(id, port)| format!("#{id} output {port}"),
        ))
        .height(210.0)
        .show_ui(ui, |ui| {
            let mut any = false;
            for (id, class, index, kind) in sources {
                any = true;
                ui.selectable_value(
                    &mut form.link,
                    Some((id.clone(), index)),
                    format!("#{id} {class} · {index} {kind}"),
                );
            }
            if !any {
                caption(ui, "No compatible output is reported in this graph.");
            }
        });
    if secondary(
        ui,
        "Apply connection",
        form.link.is_some() && form.link != link_value(current),
    )
    .clicked()
    {
        return form.link.clone().map(|(id, port)| Edit::Link(id, port));
    }
    None
}

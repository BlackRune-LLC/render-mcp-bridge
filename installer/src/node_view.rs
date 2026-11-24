use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Ui};
use serde_json::Value;

use crate::node_packages::{ManagedPack, Source, StagedPack, Target};
use crate::setup::{Installed, loopback_agent, validate_comfy_url};
use crate::{CAUTION, LIME, activity, caption, card, primary, secondary, status};

const MAX_NODES: usize = 8192;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    BuiltIn,
    Custom,
    Unreported,
}

impl Origin {
    fn from_module(module: &str) -> Self {
        let namespace = module.split('.').next().unwrap_or_default();
        match namespace {
            "nodes" | "comfy_extras" | "comfy_api_nodes" => Self::BuiltIn,
            "custom_nodes" => Self::Custom,
            _ => Self::Unreported,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "Built-in",
            Self::Custom => "Custom",
            Self::Unreported => "Origin unreported",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Node {
    id: String,
    title: String,
    origin: Origin,
    module: String,
    category: String,
    description: String,
    source: String,
    version: String,
}

fn parse_inventory(value: Value) -> Result<Vec<Node>, String> {
    const INVALID: &str = "ComfyUI returned an unreadable node inventory.";
    let entries = value.as_object().ok_or(INVALID)?;
    if entries.len() > MAX_NODES {
        return Err("The reported node inventory exceeds the size limit.".into());
    }
    let mut nodes = Vec::with_capacity(entries.len());
    for (id, info) in entries {
        if id.trim().is_empty() || id.chars().count() > 160 || !info.is_object() {
            continue;
        }
        let field = |key: &str, limit| {
            info[key]
                .as_str()
                .unwrap_or_default()
                .trim()
                .chars()
                .filter(|ch| !ch.is_control())
                .take(limit)
                .collect::<String>()
        };
        let module = field("python_module", 180);
        let title = field("display_name", 120);
        nodes.push(Node {
            id: id.clone(),
            title: if title.is_empty() { id.clone() } else { title },
            origin: Origin::from_module(&module),
            module,
            category: field("category", 120),
            description: field("description", 400),
            source: field("source", 320),
            version: field("version", 64),
        });
    }
    if !entries.is_empty() && nodes.is_empty() {
        return Err(INVALID.into());
    }
    nodes.sort_by_cached_key(|node| (node.title.to_lowercase(), node.id.clone()));
    Ok(nodes)
}

fn inspect(backend: &str) -> Result<Vec<Node>, String> {
    let origin = validate_comfy_url(backend)?;
    let mut response = loopback_agent(Duration::from_secs(6))
        .get(format!("{origin}/object_info"))
        .header("Accept", "application/json")
        .call()
        .map_err(|_| "Could not read nodes from the selected ComfyUI build.")?;
    if response.status() != 200 {
        return Err("The selected ComfyUI build did not report its nodes.".into());
    }
    let value = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_json()
        .map_err(|_| "ComfyUI returned an unreadable or oversized node inventory.")?;
    parse_inventory(value)
}

#[derive(Default)]
pub struct NodePanel {
    backend: Option<String>,
    worker: Option<Receiver<Result<Vec<Node>, String>>>,
    nodes: Vec<Node>,
    error: Option<String>,
    query: String,
    origin: Option<Origin>,
    selected: Option<usize>,
    manage_open: bool,
    management: ManagementPanel,
}

impl NodePanel {
    fn refresh(&mut self, ctx: &egui::Context) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let ctx = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        match thread::Builder::new()
            .name("render-bridge-nodes".into())
            .spawn(move || {
                let _ = sender.send(inspect(&backend));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.worker = Some(receiver);
                self.error = None;
            }
            Err(_) => self.error = Some("The node check could not start.".into()),
        }
    }

    pub fn show(&mut self, ui: &mut Ui, backend: Option<&str>, installed: Option<&Installed>) {
        let target = backend
            .zip(installed)
            .map(|(backend, installed)| Target::from_installation(backend, installed));
        let reason = target
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned();
        if reason.is_none() {
            self.show_with_target(ui, backend, target.and_then(Result::ok));
        } else {
            self.show_panel(ui, backend, None, reason.as_deref());
        }
    }

    pub(crate) fn show_with_target(
        &mut self,
        ui: &mut Ui,
        backend: Option<&str>,
        target: Option<Target>,
    ) {
        self.show_panel(ui, backend, target, None);
    }

    fn show_panel(
        &mut self,
        ui: &mut Ui,
        backend: Option<&str>,
        target: Option<Target>,
        reason: Option<&str>,
    ) {
        let target = target.filter(|target| {
            backend
                .and_then(|url| validate_comfy_url(url).ok())
                .as_deref()
                == Some(target.origin.as_str())
        });
        self.management.set_target(target);
        if self.backend.as_deref() != backend {
            self.backend = backend.map(str::to_owned);
            self.worker = None;
            self.nodes.clear();
            self.selected = None;
            self.error = None;
            self.refresh(ui.ctx());
        }
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(result) => {
                    self.worker = None;
                    self.selected = None;
                    match result {
                        Ok(nodes) => self.nodes = nodes,
                        Err(error) => {
                            self.nodes.clear();
                            self.error = Some(error);
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.error = Some("The node check did not finish. Try again.".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        ui.heading("Nodes");
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.manage_open, false, "Inventory");
            ui.selectable_value(&mut self.manage_open, true, "Manage packs");
        });
        if self.manage_open {
            self.management.show(ui, reason);
            return;
        }
        if self.backend.is_none() {
            caption(ui, "Connect a local ComfyUI build to view its nodes.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search nodes"));
            if secondary(ui, "Recheck", self.worker.is_none()).clicked() {
                self.refresh(ui.ctx());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.origin, None, "All");
            for origin in [Origin::BuiltIn, Origin::Custom, Origin::Unreported] {
                ui.selectable_value(&mut self.origin, Some(origin), origin.label());
            }
        });
        if self.worker.is_some() {
            activity(ui, "Reading node inventory…");
        }
        if let Some(error) = &self.error {
            status(ui, "Node inventory unavailable", error, CAUTION);
            return;
        }

        let query = self.query.trim().to_lowercase();
        let visible: Vec<_> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let matches_text = query.is_empty()
                    || [&node.title, &node.id, &node.category]
                        .iter()
                        .any(|part| part.to_lowercase().contains(&query));
                (matches_text && self.origin.is_none_or(|origin| node.origin == origin))
                    .then_some(index)
            })
            .collect();
        if visible.is_empty() {
            if self.worker.is_none() {
                caption(ui, "No matching nodes.");
            }
            return;
        }
        self.selected = self
            .selected
            .filter(|selected| visible.contains(selected))
            .or_else(|| visible.first().copied());

        let list = |ui: &mut Ui, selected: &mut Option<usize>| {
            caption(ui, &format!("{} nodes", visible.len()));
            egui::ScrollArea::vertical()
                .id_salt("node-inventory")
                .max_height(330.0)
                .show_rows(ui, 30.0, visible.len(), |ui, rows| {
                    for row in rows {
                        let index = visible[row];
                        let node = &self.nodes[index];
                        let label = format!("{}  ·  {}", node.title, node.origin.label());
                        if ui
                            .selectable_label(*selected == Some(index), label)
                            .on_hover_text(&node.id)
                            .clicked()
                        {
                            *selected = Some(index);
                        }
                    }
                });
        };
        let details = |ui: &mut Ui, selected: Option<usize>| {
            if let Some(node) = selected.map(|index| &self.nodes[index]) {
                self.details(ui, node);
            }
        };
        let mut selected = self.selected;
        if ui.available_width() >= 620.0 {
            ui.columns(2, |columns| {
                list(&mut columns[0], &mut selected);
                details(&mut columns[1], selected);
            });
        } else {
            list(ui, &mut selected);
            details(ui, selected);
        }
        self.selected = selected;
    }

    fn details(&self, ui: &mut Ui, node: &Node) {
        card(ui, |ui| {
            ui.heading(&node.title);
            ui.monospace(&node.id);
            caption(ui, node.origin.label());
            for (label, value) in [
                ("Category", &node.category),
                ("Module", &node.module),
                ("Version", &node.version),
            ] {
                if !value.is_empty() {
                    ui.label(format!("{label} · {value}"));
                }
            }
            if !node.description.is_empty() {
                caption(ui, &node.description);
            }
            if node.origin == Origin::Custom {
                if let Some(url) = review_source(&node.source) {
                    ui.hyperlink_to("Review source", url.as_str());
                } else {
                    caption(
                        ui,
                        "The selected backend did not report a reviewable source link.",
                    );
                }
            }
        });
    }
}

#[derive(Default)]
struct ManagementPanel {
    target: Option<Target>,
    packs: Vec<ManagedPack>,
    loaded: bool,
    repository: String,
    commit: String,
    staged: Option<StagedPack>,
    confirm_move: Option<ManagedPack>,
    trusted: bool,
    worker: Option<Receiver<Result<ManagementResult, String>>>,
    activity: &'static str,
    message: Option<(bool, String)>,
}

enum ManagementResult {
    Inventory(Vec<ManagedPack>),
    Reviewed(StagedPack),
    Changed(&'static str),
}

impl ManagementPanel {
    fn set_target(&mut self, target: Option<Target>) {
        if self.target != target {
            *self = Self {
                target,
                ..Self::default()
            };
        }
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        activity: &'static str,
        work: impl FnOnce() -> Result<ManagementResult, String> + Send + 'static,
    ) {
        let ctx = ctx.clone();
        let (sender, worker) = mpsc::channel();
        self.activity = activity;
        self.message = None;
        match thread::Builder::new()
            .name("render-bridge-node-packs".into())
            .spawn(move || {
                let _ = sender.send(work());
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(worker),
            Err(_) => {
                self.message = Some((false, "The node pack operation could not start.".into()))
            }
        }
    }

    fn refresh(&mut self, ctx: &egui::Context, target: Target) {
        self.loaded = true;
        self.start(ctx, "Reading managed packs…", move || {
            target.inventory().map(ManagementResult::Inventory)
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else {
            return;
        };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("The node pack operation did not finish.".into())
            }
        };
        self.worker = None;
        match result {
            Ok(ManagementResult::Inventory(packs)) => self.packs = packs,
            Ok(ManagementResult::Reviewed(staged)) => {
                self.trusted = false;
                self.staged = Some(staged);
            }
            Ok(ManagementResult::Changed(message)) => {
                if let Some(target) = self.target.clone() {
                    self.refresh(ctx, target);
                }
                self.message = Some((true, message.into()));
            }
            Err(error) => self.message = Some((false, error)),
        }
    }

    fn show(&mut self, ui: &mut Ui, unavailable: Option<&str>) {
        let Some(target) = self.target.clone() else {
            caption(
                ui,
                unavailable.unwrap_or(
                    "Node packs can be managed from an installed local Render Bridge setup.",
                ),
            );
            return;
        };
        self.poll(ui.ctx());
        if !self.loaded && self.worker.is_none() {
            self.refresh(ui.ctx(), target.clone());
        }
        if self.worker.is_some() {
            activity(ui, self.activity);
        }
        if let Some((success, message)) = &self.message {
            if *success {
                status(ui, "Pack folder changed", message, LIME);
            } else {
                status(ui, "Node pack operation unavailable", message, CAUTION);
            }
        }

        if ui.available_width() >= 760.0 {
            ui.columns(2, |columns| {
                self.review_panel(&mut columns[0], &target);
                self.packs_panel(&mut columns[1], &target);
            });
        } else {
            self.review_panel(ui, &target);
            self.packs_panel(ui, &target);
        }
    }

    fn review_panel(&mut self, ui: &mut Ui, target: &Target) {
        card(ui, |ui| {
            ui.heading(if self.staged.is_some() {
                "Review staged pack"
            } else {
                "Add node pack"
            });
            caption(ui, &format!("Selected backend · {}", target.origin));
            caption(ui, &target.root.display().to_string());
            if self.staged.is_some() {
                self.review_staged(ui, target);
                return;
            }
            let can_stage = self.worker.is_none() && self.confirm_move.is_none();
            ui.label("Public GitHub repository");
            ui.add_enabled(
                can_stage,
                egui::TextEdit::singleline(&mut self.repository)
                    .hint_text("https://github.com/owner/repository")
                    .char_limit(220)
                    .desired_width(f32::INFINITY),
            );
            ui.label("Full commit SHA");
            ui.add_enabled(
                can_stage,
                egui::TextEdit::singleline(&mut self.commit)
                    .hint_text("40 hexadecimal characters")
                    .char_limit(50)
                    .desired_width(f32::INFINITY),
            );
            let source = Source::parse(&self.repository, &self.commit);
            if let Ok(source) = &source {
                ui.hyperlink_to("Review pinned source", source.tree());
            } else if !self.repository.trim().is_empty() && !self.commit.trim().is_empty() {
                caption(
                    ui,
                    "Enter a public GitHub repository URL and a full commit SHA.",
                );
            }
            caption(
                ui,
                "Staging downloads the selected revision for review before it enters the node load folder.",
            );
            if primary(ui, "Stage for review", can_stage && source.is_ok()).clicked()
                && let Ok(source) = source
            {
                let target = target.clone();
                self.start(ui.ctx(), "Staging the pinned archive…", move || {
                    target.stage(source).map(ManagementResult::Reviewed)
                });
            }
        });
    }

    fn review_staged(&mut self, ui: &mut Ui, target: &Target) {
        let Some(staged) = &self.staged else {
            return;
        };
        ui.hyperlink_to(staged.source.repository(), staged.source.tree());
        ui.monospace(&staged.source.commit);
        caption(
            ui,
            &format!(
                "{} files · {}",
                staged.review.files,
                target.destination(&staged.source).display()
            ),
        );
        for (title, names) in [
            (
                "Dependency and installer files",
                &staged.review.dependencies,
            ),
            ("Startup hooks", &staged.review.startup_hooks),
        ] {
            if names.is_empty() {
                continue;
            }
            ui.label(title);
            ui.horizontal_wrapped(|ui| {
                for name in names.iter().take(12) {
                    if let Some(url) = staged.source.file(name) {
                        ui.hyperlink_to(name, url.as_str());
                    }
                }
                if names.len() > 12 {
                    caption(ui, "See the pinned source for the remaining files.");
                }
            });
        }
        caption(
            ui,
            "Review upstream instructions for any dependencies. Render Bridge stages source files only.",
        );
        status(
            ui,
            "Runs on ComfyUI startup",
            "Custom nodes can access files and network resources available to your account. Restart ComfyUI after current jobs finish.",
            CAUTION,
        );
        ui.checkbox(&mut self.trusted, "I trust this pinned revision.");
        let mut cancel = false;
        let mut install = false;
        ui.horizontal_wrapped(|ui| {
            install = primary(ui, "Install pack", self.trusted && self.worker.is_none()).clicked();
            cancel = secondary(ui, "Discard staged pack", self.worker.is_none()).clicked();
        });
        if cancel {
            self.staged = None;
            self.trusted = false;
        }
        if install && let Some(staged) = self.staged.take() {
            self.trusted = false;
            let target = target.clone();
            self.start(ui.ctx(), "Installing reviewed files…", move || {
                target.install(staged).map(|()| {
                    ManagementResult::Changed(
                        "Pack installed. Restart ComfyUI after jobs finish, then recheck Nodes.",
                    )
                })
            });
        }
    }

    fn packs_panel(&mut self, ui: &mut Ui, target: &Target) {
        card(ui, |ui| {
            ui.heading("Managed packs");
            caption(
                ui,
                "Install, enable, or disable packs here. Nodes from other installers stay in the inventory.",
            );
            if secondary(ui, "Refresh packs", self.worker.is_none()).clicked() {
                self.refresh(ui.ctx(), target.clone());
            }
            if self.packs.is_empty() {
                caption(ui, "No packs installed through Render Bridge.");
            }
            let can_change =
                self.worker.is_none() && self.staged.is_none() && self.confirm_move.is_none();
            for pack in &self.packs {
                ui.separator();
                ui.hyperlink_to(pack.source.repository(), pack.source.tree());
                ui.horizontal_wrapped(|ui| {
                    caption(
                        ui,
                        if pack.enabled {
                            "Enabled on startup"
                        } else {
                            "Disabled on startup"
                        },
                    );
                    ui.monospace(&pack.source.commit[..8]);
                    if secondary(
                        ui,
                        if pack.enabled {
                            "Disable…"
                        } else {
                            "Enable…"
                        },
                        can_change,
                    )
                    .clicked()
                    {
                        self.confirm_move = Some(pack.clone());
                    }
                });
            }
            if let Some(pack) = self.confirm_move.clone() {
                ui.separator();
                let enable = !pack.enabled;
                caption(
                    ui,
                    &format!(
                        "{} {}?",
                        if enable { "Enable" } else { "Disable" },
                        pack.source.repository()
                    ),
                );
                if !enable {
                    caption(
                        ui,
                        "ComfyUI must be running with no active or queued jobs. Render Bridge checks the queue before moving the pack.",
                    );
                }
                caption(
                    ui,
                    "Restart ComfyUI after jobs finish for this change to take effect.",
                );
                ui.horizontal_wrapped(|ui| {
                    if primary(ui, if enable { "Enable pack" } else { "Disable pack" }, self.worker.is_none()).clicked() {
                        self.confirm_move = None;
                        let target = target.clone();
                        self.start(ui.ctx(), "Moving the managed pack…", move || {
                            target.toggle(&pack.source, enable).map(|()| {
                                ManagementResult::Changed("Pack moved. Restart ComfyUI after jobs finish, then recheck Nodes.")
                            })
                        });
                    }
                    if secondary(ui, "Cancel", true).clicked() {
                        self.confirm_move = None;
                    }
                });
            }
        });
    }
}

fn review_source(source: &str) -> Option<url::Url> {
    let url = url::Url::parse(source).ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_only_reported_core_and_custom_namespaces() {
        let nodes = parse_inventory(json!({
            "SaveVideo": {"display_name": "Save Video", "python_module": "comfy_extras.nodes_video"},
            "BridgeLabs": {"python_module": "custom_nodes.bridge_labs", "version": "2.3", "source": "https://example.org/pack"},
            "Legacy": {"category": "other"},
            "LooksLikeCore": {"python_module": "comfy_extras_unrelated.nodes"}
        }))
        .unwrap();
        let by_id = |id| nodes.iter().find(|node| node.id == id).unwrap();
        assert_eq!(by_id("SaveVideo").origin, Origin::BuiltIn);
        assert_eq!(by_id("BridgeLabs").origin, Origin::Custom);
        assert_eq!(by_id("BridgeLabs").version, "2.3");
        assert_eq!(by_id("Legacy").origin, Origin::Unreported);
        assert_eq!(by_id("LooksLikeCore").origin, Origin::Unreported);
    }

    #[test]
    fn rejects_invalid_inventory_but_accepts_an_empty_registry() {
        assert!(parse_inventory(json!([])).is_err());
        assert!(parse_inventory(json!({"broken": []})).is_err());
        assert!(parse_inventory(json!({})).unwrap().is_empty());
    }

    #[test]
    fn custom_source_links_require_uncredentialed_https() {
        assert!(review_source("https://example.org/node").is_some());
        assert!(review_source("https://token@example.org/node").is_none());
        assert!(review_source("https://example.org/node?token=secret").is_none());
        assert!(review_source("file:///node").is_none());
    }
}

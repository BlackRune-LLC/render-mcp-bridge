use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Ui};
use serde_json::Value;

use crate::setup::{loopback_agent, validate_comfy_url};
use crate::{CAUTION, activity, caption, card, secondary, status};

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

    pub fn show(&mut self, ui: &mut Ui, backend: Option<&str>) {
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

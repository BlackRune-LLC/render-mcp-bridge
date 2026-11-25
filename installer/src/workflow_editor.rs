use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::setup::{fetch_json, loopback_agent, validate_comfy_url};

pub(crate) const MAX_WORKFLOW_BYTES: u64 = 2 * 1024 * 1024;
const MAX_NODES: usize = 256;
const MAX_INPUTS: usize = 128;
const MAX_OPTIONS: usize = 4096;
const MAX_UNDO: usize = 32;
const MAX_PROBLEMS: usize = 40;

type Graph = BTreeMap<String, WorkflowNode>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowNode {
    pub class_type: String,
    pub inputs: BTreeMap<String, Value>,
    #[serde(default, flatten)]
    metadata: BTreeMap<String, Value>,
}

#[derive(Clone)]
struct Edit {
    node: String,
    input: String,
    previous: Option<Value>,
}

#[derive(Clone, Default)]
pub struct WorkflowEditor {
    nodes: Graph,
    original_models: BTreeSet<String>,
    history: VecDeque<Edit>,
}

#[derive(Default)]
pub struct DependencyDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Serialize)]
pub struct DependencyReport {
    pub missing_nodes: Vec<String>,
    pub missing_models: Vec<String>,
    pub unverified_nodes: Vec<String>,
    pub unverified_models: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum InputKind {
    Integer,
    Float,
    Boolean,
    String,
    Choice(Vec<Value>),
    Link(String),
    Unknown,
}

#[derive(Clone, Debug)]
pub struct InputSchema {
    pub required: bool,
    pub kind: InputKind,
    minimum: Option<f64>,
    maximum: Option<f64>,
    combo_link: bool,
}

impl InputSchema {
    pub fn accepts_output(&self, kind: &str) -> bool {
        link_matches(kind, &self.kind, self.combo_link)
    }

    pub fn supports_links(&self) -> bool {
        self.accepts_output("*")
    }
}

#[derive(Clone, Debug)]
pub struct NodeSchema {
    pub inputs: BTreeMap<String, InputSchema>,
    pub outputs: Vec<String>,
    pub output_names: Vec<String>,
    pub output_node: bool,
    pub api_node: bool,
    pub module: Option<String>,
}

#[derive(Clone, Default)]
pub struct WorkflowSchema {
    pub nodes: BTreeMap<String, NodeSchema>,
    missing: BTreeSet<String>,
}

impl WorkflowEditor {
    pub fn import(&mut self, path: &Path) -> Result<(), String> {
        let metadata = fs::metadata(path).map_err(|_| "Could not read the workflow file.")?;
        if !metadata.is_file() || metadata.len() > MAX_WORKFLOW_BYTES {
            return Err("Choose an API workflow JSON file up to 2 MiB.".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| "Could not open the workflow file.")?
            .take(MAX_WORKFLOW_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the workflow file.")?;
        self.import_json(&bytes)
    }

    pub fn import_json(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 > MAX_WORKFLOW_BYTES {
            return Err("Choose an API workflow JSON file up to 2 MiB.".into());
        }
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let nodes: Graph = serde_json::from_slice(bytes).map_err(
            |_| "Export the workflow from ComfyUI in API format and open that JSON file.",
        )?;
        check_structure(&nodes)?;
        self.original_models = models(&nodes);
        self.nodes = nodes;
        self.history.clear();
        Ok(())
    }

    pub fn export(&self, path: &Path) -> Result<(), String> {
        let json = self.api_json()?;
        let base = path
            .parent()
            .filter(|directory| directory.is_dir())
            .ok_or("Choose a destination directory for the workflow.")?;
        let mut draft = tempfile::NamedTempFile::new_in(base)
            .map_err(|_| "Could not prepare the workflow export.")?;
        draft
            .write_all(json.as_bytes())
            .and_then(|()| draft.as_file().sync_all())
            .map_err(|_| "Could not write the workflow export.")?;
        draft.persist_noclobber(path).map_err(|_| {
            "Could not save the workflow. Choose a new filename in a writable folder.".to_owned()
        })?;
        Ok(())
    }

    pub fn nodes(&self) -> &BTreeMap<String, WorkflowNode> {
        &self.nodes
    }

    pub fn api_json(&self) -> Result<String, String> {
        check_structure(&self.nodes)?;
        let json =
            serde_json::to_string(&self.nodes).map_err(|_| "Could not encode the workflow.")?;
        if json.len() as u64 > MAX_WORKFLOW_BYTES {
            return Err("The workflow exceeds 2 MiB.".into());
        }
        Ok(json)
    }

    pub fn model_slots(
        &self,
        category: &str,
        name: &str,
        schema: &WorkflowSchema,
    ) -> Vec<(String, String)> {
        let mut slots = Vec::new();
        for (id, node) in &self.nodes {
            let Some(shape) = schema.nodes.get(&node.class_type) else {
                continue;
            };
            for (field, input) in &shape.inputs {
                if model_choice(&node.class_type, field, input, category, name).is_some() {
                    slots.push((id.clone(), field.clone()));
                }
            }
        }
        slots
    }

    pub fn assign_model(
        &mut self,
        node: &str,
        input: &str,
        category: &str,
        name: &str,
        schema: &WorkflowSchema,
    ) -> Result<DependencyDiff, String> {
        let spec = self.input_spec(node, input, schema)?;
        let choice = model_choice(&self.nodes[node].class_type, input, spec, category, name)
            .cloned()
            .ok_or("This loader does not list the selected model.")?;
        self.set_literal(node, input, choice, schema)
    }

    pub fn dependency_changes(&self) -> DependencyDiff {
        difference(&self.original_models, &models(&self.nodes))
    }

    pub fn dependency_report(&self, schema: &WorkflowSchema) -> DependencyReport {
        let mut missing_nodes = BTreeSet::new();
        let mut missing_models = BTreeSet::new();
        let mut unverified_nodes = BTreeSet::new();
        let mut unverified_models = BTreeSet::new();
        for node in self.nodes.values() {
            let spec = schema.nodes.get(&node.class_type);
            if spec.is_none() {
                if schema.missing.contains(&node.class_type) {
                    missing_nodes.insert(node.class_type.clone());
                } else {
                    unverified_nodes.insert(node.class_type.clone());
                }
            }
            for (field, value) in &node.inputs {
                let Some(model) = model_input(field, value) else {
                    continue;
                };
                match spec.and_then(|spec| spec.inputs.get(field)) {
                    Some(input) if matches!(input.kind, InputKind::Choice(_)) => {
                        if !literal_matches(value, input) {
                            missing_models.insert(model.to_owned());
                        }
                    }
                    _ => {
                        unverified_models.insert(model.to_owned());
                    }
                }
            }
        }
        DependencyReport {
            missing_nodes: missing_nodes.into_iter().collect(),
            unverified_nodes: unverified_nodes.into_iter().collect(),
            unverified_models: unverified_models
                .difference(&missing_models)
                .cloned()
                .collect(),
            missing_models: missing_models.into_iter().collect(),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        if let Some(edit) = self.history.pop_back() {
            self.restore(edit);
            true
        } else {
            false
        }
    }

    pub fn set_literal(
        &mut self,
        node: &str,
        input: &str,
        value: Value,
        schema: &WorkflowSchema,
    ) -> Result<DependencyDiff, String> {
        let target = self.input_spec(node, input, schema)?;
        if !is_literal(&value) || !literal_matches(&value, target) {
            return Err(if matches!(target.kind, InputKind::Choice(_))
                && model_input(input, &value).is_some()
            {
                "Install or select a compatible model in the selected ComfyUI build and refresh."
            } else {
                "Enter a value accepted by this input."
            }
            .into());
        }
        self.apply(node, input, Some(value))
    }

    pub fn connect(
        &mut self,
        node: &str,
        input: &str,
        source: &str,
        port: usize,
        schema: &WorkflowSchema,
    ) -> Result<DependencyDiff, String> {
        let target = self.input_spec(node, input, schema)?;
        let origin = self
            .nodes
            .get(source)
            .and_then(|item| schema.nodes.get(&item.class_type))
            .and_then(|item| item.outputs.get(port))
            .ok_or("Choose an available output port.")?;
        if !target.accepts_output(origin) {
            return Err("The output type does not match this input.".into());
        }
        self.apply(node, input, Some(serde_json::json!([source, port])))
    }

    pub fn unset(
        &mut self,
        node: &str,
        input: &str,
        schema: &WorkflowSchema,
    ) -> Result<DependencyDiff, String> {
        if self.input_spec(node, input, schema)?.required {
            return Err("This input is required by the selected node.".into());
        }
        self.apply(node, input, None)
    }

    pub fn validate(&self, schema: &WorkflowSchema) -> Vec<String> {
        if let Err(error) = check_structure(&self.nodes) {
            return vec![error];
        }
        let mut problems = Vec::new();
        let mut has_output = false;
        for (id, node) in &self.nodes {
            let Some(spec) = schema.nodes.get(&node.class_type) else {
                let message = if schema.missing.contains(&node.class_type) {
                    "is missing from the selected ComfyUI build"
                } else {
                    "could not be confirmed in the selected ComfyUI build"
                };
                problems.push(format!("Node {id} ({}) {message}.", node.class_type));
                continue;
            };
            has_output |= spec.output_node;
            for (field, input) in &spec.inputs {
                if input.required && !node.inputs.contains_key(field) {
                    problems.push(format!("Node {id} needs input {field}."));
                }
            }
            for (field, value) in &node.inputs {
                let Some(input) = spec.inputs.get(field) else {
                    problems.push(format!("Node {id} input {field} could not be confirmed."));
                    continue;
                };
                if let Some((source, port)) = link_parts(value) {
                    let kind = self
                        .nodes
                        .get(source)
                        .and_then(|item| schema.nodes.get(&item.class_type))
                        .and_then(|item| item.outputs.get(port));
                    if !kind.is_some_and(|kind| input.accepts_output(kind)) {
                        problems.push(format!(
                            "Node {id} input {field} has an incompatible or unconfirmed link."
                        ));
                    }
                } else if !literal_matches(value, input) {
                    let detail = if matches!(input.kind, InputKind::Choice(_))
                        && model_input(field, value).is_some()
                    {
                        "references a model the loader did not report. Install or select a compatible model and refresh."
                    } else {
                        "has a value the selected node does not accept."
                    };
                    problems.push(format!("Node {id} input {field} {detail}"));
                }
            }
        }
        if !has_output && problems.is_empty() {
            problems.push("The workflow needs a ComfyUI output node.".into());
        }
        problems.truncate(MAX_PROBLEMS);
        problems
    }

    fn input_spec<'a>(
        &self,
        node: &str,
        input: &str,
        schema: &'a WorkflowSchema,
    ) -> Result<&'a InputSchema, String> {
        self.nodes
            .get(node)
            .and_then(|item| schema.nodes.get(&item.class_type))
            .and_then(|item| item.inputs.get(input))
            .ok_or_else(|| "Inspect the selected ComfyUI build before editing this input.".into())
    }

    fn apply(
        &mut self,
        node: &str,
        input: &str,
        value: Option<Value>,
    ) -> Result<DependencyDiff, String> {
        let before = models(&self.nodes);
        let inputs = &mut self.nodes.get_mut(node).ok_or("Choose a node.")?.inputs;
        if inputs.get(input) == value.as_ref() {
            return Ok(DependencyDiff::default());
        }
        let previous = match value {
            Some(value) => inputs.insert(input.into(), value),
            None => inputs.remove(input),
        };
        let edit = Edit {
            node: node.into(),
            input: input.into(),
            previous,
        };
        let checked = check_structure(&self.nodes).and_then(|()| {
            if serde_json::to_vec(&self.nodes)
                .is_ok_and(|bytes| bytes.len() as u64 <= MAX_WORKFLOW_BYTES)
            {
                Ok(())
            } else {
                Err("The edit would exceed the 2 MiB workflow limit.".into())
            }
        });
        if let Err(error) = checked {
            self.restore(edit);
            return Err(error);
        }
        if self.history.len() == MAX_UNDO {
            self.history.pop_front();
        }
        self.history.push_back(edit);
        Ok(difference(&before, &models(&self.nodes)))
    }

    fn restore(&mut self, edit: Edit) {
        let Some(node) = self.nodes.get_mut(&edit.node) else {
            return;
        };
        if let Some(value) = edit.previous {
            node.inputs.insert(edit.input, value);
        } else {
            node.inputs.remove(&edit.input);
        }
    }
}

pub fn link_parts(value: &Value) -> Option<(&str, usize)> {
    let [source, port] = value.as_array()?.as_slice() else {
        return None;
    };
    Some((source.as_str()?, usize::try_from(port.as_u64()?).ok()?))
}

fn check_structure(nodes: &Graph) -> Result<(), String> {
    if nodes.is_empty() || nodes.len() > MAX_NODES {
        return Err("Choose an API workflow with 1 to 256 nodes.".into());
    }
    for (id, node) in nodes {
        if !valid_name(id) || !valid_name(&node.class_type) || node.inputs.len() > MAX_INPUTS {
            return Err("A workflow node exceeds the supported names or input limit.".into());
        }
        for (field, value) in &node.inputs {
            if !valid_name(field) {
                return Err(format!("Node {id} contains an invalid input name."));
            }
            if value.is_array()
                && !link_parts(value).is_some_and(|(source, _)| nodes.contains_key(source))
            {
                return Err(format!(
                    "Node {id} input {field} contains an invalid workflow link."
                ));
            }
        }
    }
    let mut visited = BTreeSet::new();
    let mut active = BTreeSet::new();
    if nodes
        .keys()
        .any(|id| cycle_at(id, nodes, &mut active, &mut visited))
    {
        return Err("The links would create a cycle in the workflow.".into());
    }
    Ok(())
}

fn cycle_at<'a>(
    id: &'a str,
    nodes: &'a Graph,
    active: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
) -> bool {
    if active.contains(id) {
        return true;
    }
    if !visited.insert(id) {
        return false;
    }
    active.insert(id);
    let found = nodes[id]
        .inputs
        .values()
        .filter_map(link_parts)
        .any(|(source, _)| cycle_at(source, nodes, active, visited));
    active.remove(id);
    found
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 160 && !name.chars().any(char::is_control)
}

fn models(nodes: &Graph) -> BTreeSet<String> {
    nodes
        .values()
        .flat_map(|node| node.inputs.iter())
        .filter_map(|(field, value)| model_input(field, value))
        .map(str::to_owned)
        .collect()
}

fn model_input<'a>(field: &str, value: &'a Value) -> Option<&'a str> {
    let model = value.as_str().filter(|name| is_model_file(name))?;
    let known_selector = field
        .to_ascii_lowercase()
        .split(|letter: char| !letter.is_ascii_alphanumeric())
        .any(|part| {
            matches!(
                part,
                "checkpoint"
                    | "ckpt"
                    | "clip"
                    | "clipvision"
                    | "controlnet"
                    | "lora"
                    | "model"
                    | "unet"
                    | "upscale"
                    | "upscaler"
                    | "vae"
                    | "weight"
                    | "weights"
            )
        });
    (known_selector || model_slot_category("", field).is_some()).then_some(model)
}

fn model_choice<'a>(
    class: &str,
    field: &str,
    input: &'a InputSchema,
    category: &str,
    name: &str,
) -> Option<&'a Value> {
    let InputKind::Choice(choices) = &input.kind else {
        return None;
    };
    let category = match category {
        "clip" => "text_encoders",
        "unet" => "diffusion_models",
        "vae_approx" => "vae",
        category => category,
    };
    let expected = model_slot_category(class, field);
    if expected.is_some_and(|expected| expected != category) {
        return None;
    }
    let matches = |value: &&Value| {
        value.as_str().is_some_and(|option| {
            (expected.is_some() || model_input(field, value).is_some())
                && option.split(['/', '\\']).eq(name.split(['/', '\\']))
        })
    };
    choices
        .iter()
        .find(|value| value.as_str() == Some(name) && matches(value))
        .or_else(|| choices.iter().find(matches))
}

fn model_slot_category(class: &str, field: &str) -> Option<&'static str> {
    match field {
        "ckpt_name" | "checkpoint_name" => Some("checkpoints"),
        "unet_name" | "diffusion_model_name" => Some("diffusion_models"),
        "clip_name" if class == "CLIPVisionLoader" => Some("clip_vision"),
        "clip_name" | "clip_name1" | "clip_name2" | "clip_name3" | "clip_name4" => {
            Some("text_encoders")
        }
        "clip_vision_name" => Some("clip_vision"),
        "vae_name" => Some("vae"),
        "lora_name" => Some("loras"),
        "control_net_name" | "controlnet_name" => Some("controlnet"),
        "style_model_name" => Some("style_models"),
        "gligen_name" => Some("gligen"),
        "model_path" if class == "DiffusersLoader" => Some("diffusers"),
        "model_name" => match class {
            "UpscaleModelLoader" => Some("upscale_models"),
            "LoadMoGeModel" => Some("geometry_estimation"),
            "FrameInterpolationModelLoader" => Some("frame_interpolation"),
            _ => None,
        },
        _ => None,
    }
}

fn is_model_file(value: &str) -> bool {
    value.len() <= 1024
        && !value.chars().any(char::is_control)
        && value.rsplit_once('.').is_some_and(|(_, extension)| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "safetensors" | "sft" | "ckpt" | "pt" | "pth" | "bin" | "gguf" | "onnx" | "pkl"
            )
        })
}

fn difference(before: &BTreeSet<String>, after: &BTreeSet<String>) -> DependencyDiff {
    DependencyDiff {
        added: after.difference(before).cloned().collect(),
        removed: before.difference(after).cloned().collect(),
    }
}

fn is_literal(value: &Value) -> bool {
    value.is_string() || value.is_boolean() || value.is_number()
}

fn literal_matches(value: &Value, input: &InputSchema) -> bool {
    let typed = match &input.kind {
        InputKind::Integer => value.is_i64() || value.is_u64(),
        InputKind::Float => value.is_number(),
        InputKind::Boolean => value.is_boolean(),
        InputKind::String => value.is_string(),
        InputKind::Choice(options) => options.contains(value),
        InputKind::Link(_) | InputKind::Unknown => false,
    };
    typed
        && value.as_f64().is_none_or(|number| {
            input.minimum.is_none_or(|min| number >= min)
                && input.maximum.is_none_or(|max| number <= max)
        })
}

fn link_matches(output: &str, input: &InputKind, combo_link: bool) -> bool {
    let expected = match input {
        InputKind::Integer => "INT",
        InputKind::Float => "FLOAT",
        InputKind::Boolean => "BOOLEAN",
        InputKind::String => "STRING",
        InputKind::Link(kind) => kind,
        InputKind::Choice(_) if combo_link => "COMBO",
        InputKind::Choice(_) | InputKind::Unknown => return false,
    };
    !output.is_empty()
        && output.split(',').map(str::trim).any(|actual| {
            expected
                .split(',')
                .map(str::trim)
                .any(|desired| actual == desired || actual == "*" || desired == "*")
        })
}

fn parse_schema(node: &Value) -> Option<NodeSchema> {
    let groups = node.get("input")?.as_object()?;
    let mut inputs = BTreeMap::new();
    for (group, required) in [("required", true), ("optional", false)] {
        if let Some(values) = groups.get(group) {
            for (name, spec) in values.as_object()? {
                if !valid_name(name) || inputs.len() >= MAX_INPUTS || inputs.contains_key(name) {
                    return None;
                }
                inputs.insert(name.clone(), parse_input(spec, required));
            }
        }
    }
    let outputs: Vec<_> = node
        .get("output")?
        .as_array()?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|name| valid_name(name))
                .map(str::to_owned)
        })
        .collect::<Option<_>>()?;
    if outputs.len() > MAX_INPUTS {
        return None;
    }
    let output_names = outputs
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            node["output_name"][index]
                .as_str()
                .filter(|name| valid_name(name))
                .unwrap_or(kind)
                .to_owned()
        })
        .collect();
    let api_node = match node.get("api_node") {
        Some(value) => value.as_bool()?,
        None => false,
    };
    Some(NodeSchema {
        inputs,
        outputs,
        output_names,
        output_node: node["output_node"].as_bool().unwrap_or(false),
        api_node,
        module: node["python_module"]
            .as_str()
            .filter(|module| valid_name(module))
            .map(str::to_owned),
    })
}

fn parse_input(spec: &Value, required: bool) -> InputSchema {
    let first = &spec[0];
    let options = &spec[1];
    let kind = if let Some(choices) = first.as_array() {
        parse_choices(choices)
    } else if let Some(kind) = first.as_str().filter(|name| valid_name(name)) {
        match kind {
            _ if options["forceInput"].as_bool() == Some(true)
                || options["rawLink"].as_bool() == Some(true) =>
            {
                InputKind::Link(kind.into())
            }
            "INT" => InputKind::Integer,
            "FLOAT" => InputKind::Float,
            "BOOLEAN" => InputKind::Boolean,
            "STRING" => InputKind::String,
            "COMBO" => options["options"]
                .as_array()
                .map_or(InputKind::Unknown, |choices| parse_choices(choices)),
            _ => InputKind::Link(kind.into()),
        }
    } else {
        InputKind::Unknown
    };
    InputSchema {
        required,
        kind,
        minimum: options["min"].as_f64(),
        maximum: options["max"].as_f64(),
        combo_link: first.as_str() == Some("COMBO"),
    }
}

fn parse_choices(choices: &[Value]) -> InputKind {
    if choices.len() <= MAX_OPTIONS && choices.iter().all(is_literal) {
        InputKind::Choice(choices.to_vec())
    } else {
        InputKind::Unknown
    }
}

pub fn inspect(backend: &str, editor: &WorkflowEditor) -> Result<WorkflowSchema, String> {
    let origin = validate_comfy_url(backend)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let classes: BTreeSet<_> = editor
        .nodes
        .values()
        .map(|node| node.class_type.as_str())
        .collect();
    let mut result = WorkflowSchema::default();
    for class in classes {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if class == "." || class == ".." {
            continue;
        }
        let mut endpoint = url::Url::parse(&origin).map_err(|_| "Invalid ComfyUI address.")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| "Invalid ComfyUI address.")?
            .clear()
            .push("object_info")
            .push(class);
        let agent = loopback_agent(remaining.min(Duration::from_secs(2)));
        if let Some(metadata) = fetch_json(&agent, &origin, endpoint.path()) {
            if metadata.as_object().is_some_and(|fields| fields.is_empty()) {
                result.missing.insert(class.to_owned());
            } else if let Some(schema) = metadata.get(class).and_then(parse_schema) {
                result.nodes.insert(class.to_owned(), schema);
            }
        }
    }
    Ok(result)
}

pub fn submit_test(
    backend: &str,
    editor: &WorkflowEditor,
    may_submit: impl FnOnce() -> bool,
) -> Result<String, String> {
    let origin = validate_comfy_url(backend)?;
    let schema = inspect(&origin, editor)?;
    if let Some(problem) = editor.validate(&schema).into_iter().next() {
        return Err(problem);
    }
    for (id, node) in &editor.nodes {
        let spec = &schema.nodes[&node.class_type];
        if spec.api_node
            || !spec
                .module
                .as_deref()
                .is_some_and(|module| module == "nodes" || module.starts_with("comfy_extras."))
        {
            return Err(format!(
                "Node {id} requires review and a run in ComfyUI. Render Bridge test runs use local built-in nodes."
            ));
        }
    }
    let agent = loopback_agent(Duration::from_secs(6));
    let payload = serde_json::json!({"prompt": editor.nodes});
    if !may_submit() {
        return Err(
            "The test run is paused. Check Gaming Mode and the active installation.".into(),
        );
    }
    let mut response = agent
        .post(format!("{origin}/prompt"))
        .header("Accept", "application/json")
        .send_json(payload)
        .map_err(|_| "The test queue response could not be confirmed. Check ComfyUI Jobs before trying again.")?;
    if response.status() != 200 {
        return Err("The test queue response could not be confirmed. Check ComfyUI Jobs.".into());
    }
    let status: Value = response
        .body_mut()
        .with_config()
        .limit(128 * 1024)
        .read_json()
        .map_err(|_| "The test queue response could not be confirmed. Check ComfyUI Jobs.")?;
    status["prompt_id"]
        .as_str()
        .filter(|id| valid_name(id))
        .map(str::to_owned)
        .ok_or_else(|| "The test queue response could not be confirmed. Check ComfyUI Jobs.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    fn draft() -> WorkflowEditor {
        let mut editor = WorkflowEditor::default();
        editor.import_json(br#"{"10":{"class_type":"Loader","inputs":{"weight":"a.safetensors"},"_meta":{"title":"Main loader"}},"20":{"class_type":"Sink","inputs":{"model":["10",0],"steps":4}}}"#).unwrap();
        editor
    }

    fn metadata(class: &str) -> Value {
        match class {
            "Loader" => json!({
                "input": {"required": {"weight": ["COMBO", {"options": ["a.safetensors", "b.safetensors"]}]}, "optional": {"incoming": ["MODEL"]}},
                "output": ["MODEL"], "python_module": "nodes",
            }),
            _ => json!({
                "input": {"required": {"model": ["MODEL"], "steps": ["INT", {"min": 1, "max": 12}]}, "optional": {"note": ["STRING"], "lora_name": [["grade.safetensors"]]}},
                "output": ["MODEL", "IMAGE"], "output_node": true, "python_module": "nodes",
            }),
        }
    }

    fn schema() -> WorkflowSchema {
        WorkflowSchema {
            nodes: ["Loader", "Sink"]
                .into_iter()
                .map(|class| (class.to_owned(), parse_schema(&metadata(class)).unwrap()))
                .collect(),
            ..Default::default()
        }
    }

    fn serve_requests(
        requests: Vec<(&'static str, Value)>,
    ) -> (String, thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut submitted = Vec::new();
            for (expected, reply) in requests {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "Missing request {expected}");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("Could not accept test request: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim(), expected);
                let mut content_length = 0;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                if content_length > 0 {
                    let mut bytes = vec![0; content_length];
                    reader.read_exact(&mut bytes).unwrap();
                    submitted.push(serde_json::from_slice(&bytes).unwrap());
                }
                let bytes = serde_json::to_vec(&reply).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
            submitted
        });
        (origin, handle)
    }

    fn inspection_replies(sink: Value) -> Vec<(&'static str, Value)> {
        vec![
            (
                "GET /object_info/Loader HTTP/1.1",
                json!({"Loader": metadata("Loader")}),
            ),
            ("GET /object_info/Sink HTTP/1.1", json!({"Sink": sink})),
        ]
    }

    #[test]
    fn dependency_report_distinguishes_absent_and_unverified_dependencies() {
        let mut graph = WorkflowEditor::default();
        graph
            .import_json(
                br#"{
                    "10":{"class_type":"Loader","inputs":{"weight":"absent.safetensors"}},
                    "20":{"class_type":"MissingClass","inputs":{"model_name":"unknown.safetensors"}},
                    "30":{"class_type":"UnavailableClass","inputs":{}},
                    "40":{"class_type":"Loader","inputs":{"weight":"a.safetensors"}}
                }"#,
            )
            .unwrap();
        let mut inventory = schema();
        inventory.missing.insert("MissingClass".into());
        let report = graph.dependency_report(&inventory);
        assert_eq!(report.missing_nodes, ["MissingClass"]);
        assert_eq!(report.unverified_nodes, ["UnavailableClass"]);
        assert_eq!(report.missing_models, ["absent.safetensors"]);
        assert_eq!(report.unverified_models, ["unknown.safetensors"]);
    }

    #[test]
    fn imports_and_exports_api_graph_losslessly_without_overwriting_existing_files() {
        let mut editor = draft();
        let before = editor.nodes().clone();
        assert!(
            editor
                .import_json(br#"{"nodes":[{"id":1}],"links":[]}"#)
                .is_err()
        );
        assert_eq!(editor.nodes(), &before);
        let scratch = tempfile::tempdir().unwrap();
        let export = scratch.path().join("draft.json");
        editor.export(&export).unwrap();
        let mut reloaded = WorkflowEditor::default();
        reloaded.import(&export).unwrap();
        assert_eq!(reloaded.nodes(), &before);
        assert!(editor.export(&export).is_err());
    }

    #[test]
    fn checks_selector_values_literal_ranges_links_and_cycles_then_undoes_changes() {
        let mut editor = draft();
        let schema = schema();
        assert!(editor.validate(&schema).is_empty());
        assert!(
            editor
                .set_literal("10", "weight", json!(["20", 0]), &schema)
                .is_err()
        );
        assert!(
            editor
                .set_literal("20", "steps", json!(13), &schema)
                .is_err()
        );
        assert!(editor.connect("20", "model", "20", 1, &schema).is_err());
        assert!(editor.connect("10", "incoming", "20", 0, &schema).is_err());
        assert!(!editor.can_undo());

        let diff = editor
            .set_literal("10", "weight", json!("b.safetensors"), &schema)
            .unwrap();
        assert_eq!(diff.added, ["b.safetensors"]);
        assert_eq!(diff.removed, ["a.safetensors"]);
        assert_eq!(editor.dependency_changes().added, ["b.safetensors"]);
        assert!(editor.undo());
        assert!(editor.dependency_changes().added.is_empty());
        assert!(!editor.can_undo());
    }

    #[test]
    fn rejects_malformed_or_dangling_link_without_destroying_the_draft() {
        let mut editor = draft();
        for text in [
            br#"{"1":{"class_type":"Node","inputs":{"a":["2",0]}}}"#.as_slice(),
            br#"{"1":{"class_type":"Node","inputs":{"a":[1,0]}}}"#.as_slice(),
            br#"{"1":{"class_type":"Node","inputs":{"a":["1",0]}}}"#.as_slice(),
        ] {
            assert!(editor.import_json(text).is_err());
        }
        assert_eq!(editor.nodes().len(), 2);
    }

    #[test]
    fn accepts_exported_comfy_v3_combo_links() {
        let mut editor = WorkflowEditor::default();
        editor
            .import_json(
                br#"{
            "10":{"class_type":"ComboSwitch","inputs":{}},
            "20":{"class_type":"MethodSink","inputs":{"method":["10",0]}}
        }"#,
            )
            .unwrap();
        let mut schema = WorkflowSchema {
            nodes: [
                ("ComboSwitch".to_owned(), parse_schema(&json!({
                    "input":{"required":{}}, "output":["COMBO"], "python_module":"comfy_extras.nodes_logic"
                })).unwrap()),
                ("MethodSink".to_owned(), parse_schema(&json!({
                    "input":{"required":{"method":["COMBO",{"options":["a","b"]}]}},
                    "output":[], "output_node":true, "python_module":"nodes"
                })).unwrap()),
            ].into(),
            ..Default::default()
        };
        assert_eq!(editor.validate(&schema), Vec::<String>::new());
        assert!(editor.connect("20", "method", "10", 0, &schema).is_ok());

        schema.nodes.insert(
            "MethodSink".into(),
            parse_schema(&json!({
                "input":{"required":{"method":[["a","b"]]}},
                "output":[], "output_node":true, "python_module":"nodes"
            }))
            .unwrap(),
        );
        assert!(!editor.validate(&schema).is_empty());
    }

    #[test]
    fn assigns_an_installed_model_only_to_matching_loader_slots() {
        let mut editor = WorkflowEditor::default();
        editor
            .import_json(
                br#"{
            "1":{"class_type":"CLIPLoader","inputs":{"clip_name":"old.safetensors"}},
            "2":{"class_type":"CLIPVisionLoader","inputs":{"clip_name":"old.safetensors"}},
            "3":{"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"old.safetensors"}}
        }"#,
            )
            .unwrap();
        let choices = json!(["old.safetensors", "pack\\replacement.safetensors"]);
        let schema = WorkflowSchema {
            nodes: [
                ("CLIPLoader", "clip_name"),
                ("CLIPVisionLoader", "clip_name"),
                ("CheckpointLoaderSimple", "ckpt_name"),
            ]
            .into_iter()
            .map(|(class, input)| {
                let metadata = json!({
                    "input":{"required":{(input): ["COMBO", {"options": choices}]}},
                    "output":["CLIP"],
                    "output_name":["Encoder"],
                    "output_node":false,
                    "python_module":"nodes"
                });
                (class.to_owned(), parse_schema(&metadata).unwrap())
            })
            .collect(),
            ..Default::default()
        };
        assert_eq!(
            editor.model_slots("text_encoders", "pack/replacement.safetensors", &schema),
            [("1".into(), "clip_name".into())]
        );
        assert!(
            editor
                .model_slots("clip", "replacement.safetensors", &schema)
                .is_empty()
        );
        assert!(
            editor
                .assign_model(
                    "2",
                    "clip_name",
                    "clip",
                    "pack/replacement.safetensors",
                    &schema
                )
                .is_err()
        );
        editor
            .assign_model(
                "1",
                "clip_name",
                "clip",
                "pack/replacement.safetensors",
                &schema,
            )
            .unwrap();

        let json: Value = serde_json::from_str(&editor.api_json().unwrap()).unwrap();
        assert_eq!(
            json["1"]["inputs"]["clip_name"],
            "pack\\replacement.safetensors"
        );
        assert_eq!(json["2"]["inputs"]["clip_name"], "old.safetensors");
        assert!(editor.undo());
        assert_eq!(editor.nodes()["1"].inputs["clip_name"], "old.safetensors");
    }

    #[test]
    fn free_text_with_a_weight_extension_is_not_a_model_dependency() {
        let mut editor = draft();
        let diff = editor
            .set_literal("20", "note", json!("example.safetensors"), &schema())
            .unwrap();
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(editor.dependency_changes().added.is_empty());
    }

    #[test]
    fn only_optional_inputs_can_be_removed_and_the_removal_can_be_undone() {
        let mut editor = draft();
        let schema = schema();
        assert!(editor.unset("20", "model", &schema).is_err());
        let noop = editor.unset("20", "lora_name", &schema).unwrap();
        assert!(noop.added.is_empty() && noop.removed.is_empty());
        assert!(!editor.can_undo());

        editor
            .set_literal("20", "lora_name", json!("grade.safetensors"), &schema)
            .unwrap();
        let diff = editor.unset("20", "lora_name", &schema).unwrap();
        assert_eq!(diff.removed, ["grade.safetensors"]);
        assert!(editor.validate(&schema).is_empty());
        assert!(editor.undo());
        assert_eq!(editor.dependency_changes().added, ["grade.safetensors"]);
    }

    #[test]
    fn explicit_test_submits_the_validated_graph_to_the_selected_loopback_queue() {
        let editor = draft();
        let mut replies = inspection_replies(metadata("Sink"));
        replies.push(("POST /prompt HTTP/1.1", json!({"prompt_id": "fixture-job"})));
        let (origin, server) = serve_requests(replies);
        assert_eq!(
            submit_test(&origin, &editor, || true).unwrap(),
            "fixture-job"
        );
        assert_eq!(server.join().unwrap(), [json!({"prompt": editor.nodes()})]);
    }

    #[test]
    fn test_queue_rejects_provider_and_custom_nodes_before_posting() {
        for (module, api_node) in [
            ("nodes", true),
            ("custom_nodes.provider", false),
            ("", false),
        ] {
            let mut sink = metadata("Sink");
            sink["python_module"] = json!(module);
            sink["api_node"] = json!(api_node);
            let (origin, server) = serve_requests(inspection_replies(sink));
            assert!(
                submit_test(&origin, &draft(), || true)
                    .unwrap_err()
                    .contains("requires review")
            );
            assert!(server.join().unwrap().is_empty());
        }
    }

    #[test]
    fn test_queue_rechecks_the_submission_gate_after_inspecting_the_backend() {
        let (origin, server) = serve_requests(inspection_replies(metadata("Sink")));
        assert!(
            submit_test(&origin, &draft(), || false)
                .unwrap_err()
                .contains("paused")
        );
        assert!(server.join().unwrap().is_empty());
    }
}

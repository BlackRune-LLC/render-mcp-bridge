use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pipeline;
use crate::setup::{existing_install_at, installed_comfy_url, validate_comfy_url};
use crate::workflow_editor::{self, DependencyReport, MAX_WORKFLOW_BYTES, WorkflowEditor};

const MAX_REQUEST_BYTES: u64 = MAX_WORKFLOW_BYTES + 1024;
const BACKEND_MISMATCH: &str =
    "Render Bridge and the MCP service must select the same ComfyUI address.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    name: String,
    workflow: Value,
    comfy_url: String,
}

#[derive(Serialize)]
struct Receipt {
    id: String,
    name: String,
    #[serde(flatten)]
    dependencies: DependencyReport,
    validation_issues: Vec<String>,
}

fn read_request(input: impl Read) -> Result<Request, String> {
    let mut bytes = Vec::new();
    input
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the pipeline request.")?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("The pipeline request exceeds the size limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid pipeline request JSON.".into())
}

fn check_backend(data_dir: &Path, expected: &str) -> Result<(), String> {
    let installed = existing_install_at(data_dir)
        .ok_or("Could not read the selected Render Bridge installation.")?;
    if installed_comfy_url(&installed).as_deref() != Some(expected) {
        return Err(BACKEND_MISMATCH.into());
    }
    Ok(())
}

fn add(data_dir: &Path, request: Request) -> Result<Receipt, String> {
    #[cfg(windows)]
    let _data_pins = crate::uninstall::pin_local_directories(data_dir)?;
    let expected_backend = validate_comfy_url(&request.comfy_url)?;
    check_backend(data_dir, &expected_backend)?;

    let mut graph = WorkflowEditor::default();
    let bytes =
        serde_json::to_vec(&request.workflow).map_err(|_| "Could not read the workflow.")?;
    graph.import_json(&bytes)?;
    let inventory = workflow_editor::inspect(&expected_backend, &graph)?;
    let saved = pipeline::save_new_checked(data_dir, request.name.trim(), &graph, || {
        check_backend(data_dir, &expected_backend)
    })?;

    Ok(Receipt {
        id: saved.name.clone(),
        name: saved.name,
        dependencies: graph.dependency_report(&inventory),
        validation_issues: graph.validate(&inventory),
    })
}

pub(crate) fn run(data_dir: Option<&Path>) -> ExitCode {
    let response = data_dir
        .ok_or_else(|| "Select one absolute Render Bridge installation directory.".into())
        .and_then(|data_dir| {
            let request = read_request(io::stdin().lock())?;
            add(data_dir, request)
        });
    let mut output = io::stdout().lock();
    match response {
        Ok(receipt) => {
            if serde_json::to_writer(&mut output, &receipt).is_ok() && output.flush().is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            let _ = serde_json::to_writer(&mut output, &serde_json::json!({ "error": error }));
            ExitCode::FAILURE
        }
    }
}
